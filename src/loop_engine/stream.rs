//! Provider-agnostic streaming-output parsing for the autonomous agent loop.
//!
//! LLM CLIs emit `--output-format`-style JSONL on stdout, but the schema is
//! provider-specific: Claude streams `{"type":"assistant",...}` blocks plus a
//! terminal `{"type":"result","result":...}`, while Grok streams
//! `{"type":"text"|"thought","data":...}` chunks with a terminal
//! `{"type":"end",...}` and **no** result line.
//!
//! This module decouples *what a provider's lines mean* (a [`StreamFormat`]
//! impl mapping each parsed JSON value to normalized [`StreamEvent`]s and
//! declaring its output-derivation policy) from *the shared mechanics* of
//! teeing live output, building the conversation transcript, accumulating the
//! result string, and arming the completion-grace window ([`drive_stream`] +
//! [`accumulate`]). Adding a provider is a new `StreamFormat` impl wired at the
//! runner's spawn site — no change to the driver, accumulation, or grace logic.
//!
//! `ClaudeStreamFormat` lives in `claude.rs` (next to the Claude-specific JSON
//! helpers it reuses); `GrokStreamFormat` lives here.

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::loop_engine::claude::{
    MAX_TOOL_RESULT_BYTES, MAX_TOOL_USE_BYTES, append_capped, is_pty_read_eof, truncate_bytes,
};
use crate::loop_engine::output_parsing::parse_completed_tasks;
use crate::loop_engine::watchdog::POST_COMPLETION_GRACE_SECS;
use crate::output::ui;

/// Soft cap on the accumulated assistant output buffer (the source of
/// `RunnerResult.output` for providers that lack an authoritative final-result
/// line, e.g. Grok). Truncation keeps the **tail** — completion tags
/// (`<task-status>`, `<completed>`) are emitted at end-of-turn, so the head is
/// the safe end to drop. Larger than `MAX_CONVERSATION_BYTES`: this buffer must
/// retain enough trailing context for the completion ladder's tag scan even on
/// a verbose review, where the conversation transcript can be lossy.
pub(crate) const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// One normalized event produced from a provider's stdout line.
///
/// The channel each event feeds is fixed (so providers can't accidentally
/// diverge): `AssistantText` is both displayed and accumulated; `LiveOnlyText`
/// is displayed but never accumulated (Claude tees an errored message's text
/// for visibility while suppressing it from the transcript — see
/// `ClaudeStreamFormat`); `Thought` is display-only; tool/error events feed the
/// conversation transcript only; `FinalResult`/`PermissionDenials` feed the
/// terminal result.
pub(crate) enum StreamEvent {
    /// Assistant reply text: teed live AND accumulated into output + conversation.
    AssistantText(String),
    /// Teed live for visibility, NEVER accumulated (errored-assistant text).
    LiveOnlyText(String),
    /// Reasoning/thinking text: teed live, never accumulated.
    Thought(String),
    /// Tool invocation — conversation transcript only.
    ToolUse { name: String, input: String },
    /// Tool result — conversation transcript only.
    ToolResult(String),
    /// Authoritative final output string (e.g. Claude's `result.result`).
    FinalResult(String),
    /// Permission-denial JSON values carried through to `RunnerResult`.
    PermissionDenials(Vec<Value>),
    /// Error text — conversation transcript only (`[Error: ...]`), never teed.
    Error(String),
}

/// Mutable fold state shared by the production driver and the test helpers.
#[derive(Default)]
pub(crate) struct Accumulator {
    /// Concatenated `AssistantText` (tail-capped). The output source for
    /// providers without a `FinalResult` line.
    pub(crate) assistant_buf: String,
    /// Authoritative final result, if the provider emits one.
    pub(crate) final_result: Option<String>,
    /// Formatted conversation transcript (capped at `MAX_CONVERSATION_BYTES`).
    pub(crate) conversation: String,
    /// Permission-denial values.
    pub(crate) denials: Vec<Value>,
}

/// Provider-specific stdout interpretation.
///
/// `parse_value` writes events into a sink rather than returning a `Vec` so the
/// hot path (every stdout line — for Grok, every token fragment) allocates no
/// intermediate collection. `derive_output` makes each provider's
/// output-derivation policy explicit rather than inferring it from event
/// presence: a future provider must *state* whether its output is an
/// authoritative final line or the accumulated stream, not inherit a default
/// that might silently drop its text.
pub(crate) trait StreamFormat {
    /// Map one parsed JSON line into zero or more events via `sink`.
    fn parse_value(&self, val: &Value, sink: &mut dyn FnMut(StreamEvent));

    /// Derive the final output string from the folded accumulator.
    fn derive_output(&self, acc: &Accumulator) -> String;

    /// Whether displayed text arrives as token fragments that should be buffered
    /// to newline boundaries before teeing (true), vs. complete units teed as-is
    /// (false). Token-streaming providers (Grok emits one `text` chunk per token)
    /// set true so the live console shows whole lines instead of one token per
    /// line; block providers (Claude tees a whole message at once) keep the
    /// default and tee each event immediately.
    fn line_buffer_tee(&self) -> bool {
        false
    }
}

/// Fold a single event into the accumulator. Pure: no I/O, no atomics. This is
/// the accumulation core shared by `drive_stream` and the `#[cfg(test)]`
/// value-iterator helpers, so the existing parser tests pin its behavior.
pub(crate) fn accumulate(acc: &mut Accumulator, ev: StreamEvent) {
    match ev {
        StreamEvent::AssistantText(t) => {
            append_output_tail(&mut acc.assistant_buf, &t);
            append_capped(&mut acc.conversation, &t);
            append_capped(&mut acc.conversation, "\n");
        }
        // Display-only events never touch the accumulator.
        StreamEvent::LiveOnlyText(_) | StreamEvent::Thought(_) => {}
        StreamEvent::ToolUse { name, input } => {
            let truncated = truncate_bytes(&input, MAX_TOOL_USE_BYTES);
            append_capped(
                &mut acc.conversation,
                &format!("[Tool: {}] {}\n", name, truncated),
            );
        }
        StreamEvent::ToolResult(content) => {
            let truncated = truncate_bytes(&content, MAX_TOOL_RESULT_BYTES);
            append_capped(&mut acc.conversation, &format!("[Result: {}]\n", truncated));
        }
        StreamEvent::FinalResult(s) => {
            acc.final_result = Some(s);
        }
        StreamEvent::PermissionDenials(d) => {
            acc.denials.extend(d);
        }
        StreamEvent::Error(e) => {
            append_capped(&mut acc.conversation, &format!("[Error: {}]\n", e));
        }
    }
}

/// Append `s` to `buf`, then bound memory by dropping the front when the buffer
/// exceeds `2 * MAX_OUTPUT_BYTES` down to the trailing `MAX_OUTPUT_BYTES`.
/// Amortized O(1): a front-drop happens at most once per `MAX_OUTPUT_BYTES` of
/// growth. Char-boundary-safe so the buffer stays valid UTF-8.
fn append_output_tail(buf: &mut String, s: &str) {
    buf.push_str(s);
    if buf.len() > 2 * MAX_OUTPUT_BYTES {
        let mut start = buf.len() - MAX_OUTPUT_BYTES;
        while start < buf.len() && !buf.is_char_boundary(start) {
            start += 1;
        }
        buf.replace_range(..start, "");
    }
}

/// Return the trailing `max` bytes of `s` (char-boundary-safe), keeping the end
/// where completion tags live. Used by stream-accumulating providers' output.
pub(crate) fn tail_truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Human-visible tee channels. Tracked so a buffered tee flushes its pending
/// partial line when the provider switches between reasoning and reply text,
/// instead of merging a thought's tail with the reply's head on one line.
#[derive(PartialEq, Clone, Copy)]
enum TeeChannel {
    Assistant,
    Thought,
}

/// Drain complete lines (split on `\n`) from `buf`, returning each WITHOUT its
/// trailing newline and leaving any trailing partial line in `buf`.
fn drain_complete_lines(buf: &mut String) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(nl) = buf.find('\n') {
        lines.push(buf[..nl].to_string());
        buf.replace_range(..=nl, "");
    }
    lines
}

/// Tee one (possibly multi-line) text block, dropping lines that are empty or
/// whitespace-only. Streamed reasoning/reply text is dense with blank lines
/// (Grok in particular emits a blank line between almost every sentence); each
/// would otherwise render as a prefix-only console row (`[slot 0]`) that adds
/// noise without information. Display-only: the accumulator and transcript
/// (built from the same events in `accumulate`) are untouched, so suppression
/// here cannot affect output derivation or the completion ladder.
fn emit_tee_block(slot_label: Option<&str>, text: &str) {
    for line in tee_visible_lines(text) {
        ui::emit_prefixed(slot_label, line);
    }
}

/// The lines of `text` that survive blank-line suppression, in order. Pure
/// (the I/O lives in `emit_tee_block`) so the suppression policy is unit-tested
/// without touching real stderr.
fn tee_visible_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|line| !line.trim().is_empty())
}

/// Tee one text chunk. When `buffered`, accumulate into `buf` and emit only
/// complete lines (flushing the partial first on a channel switch); otherwise
/// emit the chunk immediately (block-provider behavior). Both paths route
/// through `emit_tee_block`, which drops blank/whitespace-only lines.
fn tee_push(
    buf: &mut String,
    channel: &mut Option<TeeChannel>,
    buffered: bool,
    slot_label: Option<&str>,
    ch: TeeChannel,
    text: &str,
) {
    if !buffered {
        emit_tee_block(slot_label, text);
        return;
    }
    if *channel != Some(ch) {
        if !buf.is_empty() {
            emit_tee_block(slot_label, &std::mem::take(buf));
        }
        *channel = Some(ch);
    }
    buf.push_str(text);
    for line in drain_complete_lines(buf) {
        emit_tee_block(slot_label, &line);
    }
}

/// Read provider stdout, tee live output, build the transcript, and return
/// `(output, Some(conversation), permission_denials)`.
///
/// Generic over the concrete `StreamFormat` so dispatch is static (monomorphized
/// per provider at the spawn site) — no `&dyn` on the per-line hot path.
pub(crate) fn drive_stream<F: StreamFormat>(
    reader: BufReader<impl Read>,
    format: &F,
    target_task_id: Option<&str>,
    completion_epoch: &AtomicU64,
    slot_label: Option<&str>,
) -> (String, Option<String>, Vec<Value>) {
    let buffered_tee = format.line_buffer_tee();
    let mut acc = Accumulator::default();
    // Live-tee state for fragment-streaming providers (no-op when !buffered_tee).
    let mut tee_buf = String::new();
    let mut tee_channel: Option<TeeChannel> = None;
    {
        let mut sink = |ev: StreamEvent| {
            let is_assistant_text = matches!(ev, StreamEvent::AssistantText(_));
            // Tee the human-visible channels live, before folding.
            match &ev {
                StreamEvent::AssistantText(t) | StreamEvent::LiveOnlyText(t) => tee_push(
                    &mut tee_buf,
                    &mut tee_channel,
                    buffered_tee,
                    slot_label,
                    TeeChannel::Assistant,
                    t,
                ),
                StreamEvent::Thought(t) => tee_push(
                    &mut tee_buf,
                    &mut tee_channel,
                    buffered_tee,
                    slot_label,
                    TeeChannel::Thought,
                    t,
                ),
                _ => {}
            }
            accumulate(&mut acc, ev);
            // Arm the post-completion grace once the accumulated buffer contains
            // the target's `<completed>` tag — scanning the buffer (not a single
            // chunk) catches tags split across token fragments.
            if is_assistant_text {
                arm_completion_grace(&acc.assistant_buf, target_task_id, completion_epoch);
            }
        };

        for line_result in reader.lines() {
            match line_result {
                Ok(line) => match serde_json::from_str::<Value>(&line) {
                    Ok(val) => format.parse_value(&val, &mut sink),
                    Err(_) => ui::emit_prefixed(
                        slot_label,
                        "Warning: malformed stream-json line (not valid JSON)",
                    ),
                },
                Err(e) if is_pty_read_eof(&e) => break,
                Err(e) => {
                    ui::emit_prefixed(slot_label, &format!("Warning: error reading stdout: {}", e));
                    break;
                }
            }
        }
    }

    // Flush any trailing partial line the provider never newline-terminated.
    if !tee_buf.is_empty() {
        emit_tee_block(slot_label, &tee_buf);
    }

    let output = format.derive_output(&acc);
    (output, Some(acc.conversation), acc.denials)
}

/// Arm the completion-grace window if the accumulated assistant buffer contains
/// `<completed>TARGET</completed>` for the current target task.
///
/// First observation wins (epoch CAS). Scans only a trailing window large enough
/// to hold one full tag — cheap even when called per token fragment, and the
/// `epoch != 0` short-circuit makes the steady state free once armed. Matching
/// is exact-equality on the extracted ID (via `parse_completed_tasks`), so an
/// adjacent ID like `TARGET2` does not false-arm.
fn arm_completion_grace(buf: &str, target_task_id: Option<&str>, completion_epoch: &AtomicU64) {
    if completion_epoch.load(Ordering::Acquire) != 0 {
        return; // already armed — first observation wins
    }
    let Some(target) = target_task_id else {
        return;
    };

    // Window must hold `<completed>` + id + `</completed>` even when the tag
    // straddles a chunk boundary; a small slack covers surrounding whitespace.
    let window_bytes = "<completed></completed>".len() + target.len() + 64;
    let win = window_bytes.min(buf.len());
    let mut start = buf.len() - win;
    while start > 0 && !buf.is_char_boundary(start) {
        start -= 1;
    }
    let window = &buf[start..];

    if parse_completed_tasks(window).iter().any(|id| id == target) {
        // Saturate to 1 so the `0 == not armed` sentinel stays unambiguous even
        // on a host whose clock reads pre-epoch.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .max(1);
        if completion_epoch
            .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            eprintln!(
                "[completion] saw <completed>{}</completed> in stream — {}s grace window begins",
                target, POST_COMPLETION_GRACE_SECS,
            );
        }
    }
}

/// Grok CLI stream format: `--output-format streaming-json` emits
/// `{"type":"text"|"thought","data":"..."}` chunks (assistant text streamed as
/// many token fragments) and a terminal `{"type":"end","stopReason":...}`. No
/// `result` line, so output is the accumulated text.
///
/// UTF-8 assumption: each `data` is a complete valid-UTF-8 JSON string (a
/// requirement of valid JSON), so concatenating chunks cannot split a codepoint.
pub(crate) struct GrokStreamFormat;

impl StreamFormat for GrokStreamFormat {
    fn parse_value(&self, val: &Value, sink: &mut dyn FnMut(StreamEvent)) {
        match val.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(data) = val.get("data").and_then(|d| d.as_str()) {
                    sink(StreamEvent::AssistantText(data.to_string()));
                }
            }
            Some("thought") => {
                if let Some(data) = val.get("data").and_then(|d| d.as_str()) {
                    sink(StreamEvent::Thought(data.to_string()));
                }
            }
            // "end" (terminal marker) and unknown types contribute nothing.
            _ => {}
        }
    }

    fn derive_output(&self, acc: &Accumulator) -> String {
        // No authoritative final line: output is the accumulated assistant text,
        // tail-capped so a verbose review can't grow the buffer unbounded while
        // still preserving the trailing completion tag.
        tail_truncate(&acc.assistant_buf, MAX_OUTPUT_BYTES)
    }

    fn line_buffer_tee(&self) -> bool {
        true // grok streams assistant text one token per line
    }
}

/// Codex CLI `exec --json` stream format.
///
/// Output is accumulated from completed assistant messages. Terminal
/// `turn.completed` usage records are intentionally ignored for output.
pub(crate) struct CodexStreamFormat;

impl StreamFormat for CodexStreamFormat {
    fn parse_value(&self, val: &Value, sink: &mut dyn FnMut(StreamEvent)) {
        match val.get("type").and_then(|t| t.as_str()) {
            Some("item.completed") => {
                let Some(item) = val.get("item") else {
                    return;
                };
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("agent_message") => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            sink(StreamEvent::AssistantText(text.to_string()));
                        }
                    }
                    Some("command_execution") => {
                        let command = item
                            .get("command")
                            .and_then(|c| c.as_str())
                            .unwrap_or("command_execution");
                        let output = item
                            .get("aggregated_output")
                            .and_then(|o| o.as_str())
                            .unwrap_or("");
                        sink(StreamEvent::ToolUse {
                            name: "command_execution".to_string(),
                            input: command.to_string(),
                        });
                        sink(StreamEvent::ToolResult(output.to_string()));
                    }
                    _ => {}
                }
            }
            Some("item.started") => {
                if let Some(item) = val.get("item")
                    && item.get("type").and_then(|t| t.as_str()) == Some("command_execution")
                {
                    let command = item
                        .get("command")
                        .and_then(|c| c.as_str())
                        .unwrap_or("command_execution");
                    sink(StreamEvent::ToolUse {
                        name: "command_execution".to_string(),
                        input: command.to_string(),
                    });
                }
            }
            Some("error") => {
                if let Some(message) = val.get("message").and_then(|m| m.as_str()) {
                    sink(StreamEvent::Error(message.to_string()));
                }
            }
            Some("turn.failed") => {
                let message = val
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("turn failed");
                sink(StreamEvent::Error(message.to_string()));
            }
            _ => {}
        }
    }

    fn derive_output(&self, acc: &Accumulator) -> String {
        tail_truncate(&acc.assistant_buf, MAX_OUTPUT_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::claude::ClaudeStreamFormat;

    /// Fold a provider's JSONL lines through the accumulation core (no tee/grace),
    /// mirroring the production fold for assertion in tests.
    fn run_format<F: StreamFormat>(format: &F, lines: &[&str]) -> (String, String, Vec<Value>) {
        let mut acc = Accumulator::default();
        {
            let mut sink = |ev| accumulate(&mut acc, ev);
            for line in lines {
                let val: Value = serde_json::from_str(line).expect("valid JSON fixture line");
                format.parse_value(&val, &mut sink);
            }
        }
        let output = format.derive_output(&acc);
        (
            output,
            acc.conversation.clone(),
            std::mem::take(&mut acc.denials),
        )
    }

    #[test]
    fn grok_text_chunks_reassemble_into_output() {
        // Tag split across token fragments, interleaved thoughts, terminal end.
        let lines = [
            r#"{"type":"thought","data":"let me think"}"#,
            r#"{"type":"text","data":"<task-"}"#,
            r#"{"type":"text","data":"status>REVIEW-1:done</task-"}"#,
            r#"{"type":"text","data":"status>"}"#,
            r#"{"type":"end","stopReason":"EndTurn"}"#,
        ];
        let (output, _conv, _d) = run_format(&GrokStreamFormat, &lines);
        assert_eq!(output, "<task-status>REVIEW-1:done</task-status>");
        assert!(
            crate::loop_engine::detection::extract_status_updates(&output)
                .iter()
                .any(|u| u.task_id == "REVIEW-1"),
            "completion ladder must see the reassembled tag"
        );
    }

    /// Regression lock against grok CLI schema drift: a real captured
    /// `--output-format streaming-json` stdout (thought + text chunks + a
    /// terminal `end`) must reassemble to the assistant's reply. If a grok
    /// version changes the line shape, this fails loudly instead of silently
    /// dropping output again. Re-capture with
    /// `grok --output-format streaming-json --permission-mode auto --model grok-build -p '...'`.
    #[test]
    fn grok_real_capture_fixture_reassembles() {
        let raw = include_str!("../../tests/fixtures/grok_stream.jsonl");
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        let (output, _conv, _d) = run_format(&GrokStreamFormat, &lines);
        assert_eq!(
            output, "<task-status>TEST-1:done</task-status>",
            "real grok stdout capture must reassemble to the emitted assistant text"
        );
    }

    #[test]
    fn drain_complete_lines_holds_trailing_partial() {
        // Token fragments accumulate; only newline-terminated lines drain.
        let mut buf = String::new();
        buf.push_str("Hel");
        assert!(drain_complete_lines(&mut buf).is_empty());
        assert_eq!(buf, "Hel");
        buf.push_str("lo\nwor");
        assert_eq!(drain_complete_lines(&mut buf), vec!["Hello".to_string()]);
        assert_eq!(buf, "wor", "trailing partial line is retained");
        buf.push_str("ld\n");
        assert_eq!(drain_complete_lines(&mut buf), vec!["world".to_string()]);
        assert_eq!(buf, "", "fully consumed on trailing newline");
    }

    #[test]
    fn drain_complete_lines_multiple_in_one_chunk() {
        let mut buf = "a\nb\nc".to_string();
        assert_eq!(
            drain_complete_lines(&mut buf),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(buf, "c");
    }

    #[test]
    fn tee_visible_lines_drops_blank_and_whitespace_only() {
        // The dense blank-line reasoning text the live tee was rendering as
        // prefix-only rows: every other line is empty or whitespace-only.
        let block = "First, check the branch.\n\nUse run_terminal.\n   \nThen run the gate.";
        let kept: Vec<&str> = tee_visible_lines(block).collect();
        assert_eq!(
            kept,
            vec![
                "First, check the branch.",
                "Use run_terminal.",
                "Then run the gate."
            ],
            "blank and whitespace-only lines are suppressed; content lines kept in order"
        );
    }

    #[test]
    fn tee_visible_lines_empty_block_emits_nothing() {
        // An empty text event (Grok partial flush, Claude empty content block)
        // produces no prefix-only line at all.
        assert_eq!(tee_visible_lines("").count(), 0);
        assert_eq!(tee_visible_lines("\n\n").count(), 0);
    }

    #[test]
    fn format_line_buffer_tee_flags() {
        // Grok opts into buffering; Claude keeps the immediate-tee default.
        assert!(GrokStreamFormat.line_buffer_tee());
        assert!(!ClaudeStreamFormat.line_buffer_tee());
    }

    #[test]
    fn grok_thought_excluded_from_output() {
        let lines = [
            r#"{"type":"thought","data":"reasoning that must not leak into output"}"#,
            r#"{"type":"text","data":"actual answer"}"#,
        ];
        let (output, _conv, _d) = run_format(&GrokStreamFormat, &lines);
        assert_eq!(output, "actual answer");
    }

    #[test]
    fn grok_completed_tag_split_across_chunks_arms_grace() {
        let epoch = AtomicU64::new(0);
        // Two text chunks that only form the tag once concatenated.
        let mut acc = Accumulator::default();
        accumulate(
            &mut acc,
            StreamEvent::AssistantText("<completed>RE".to_string()),
        );
        arm_completion_grace(&acc.assistant_buf, Some("REVIEW-1"), &epoch);
        assert_eq!(epoch.load(Ordering::Acquire), 0, "partial tag must not arm");
        accumulate(
            &mut acc,
            StreamEvent::AssistantText("VIEW-1</completed>".to_string()),
        );
        arm_completion_grace(&acc.assistant_buf, Some("REVIEW-1"), &epoch);
        assert_ne!(epoch.load(Ordering::Acquire), 0, "completed tag must arm");
    }

    #[test]
    fn grace_does_not_arm_on_adjacent_id() {
        let epoch = AtomicU64::new(0);
        let buf = "<completed>REVIEW-12</completed>";
        arm_completion_grace(buf, Some("REVIEW-1"), &epoch);
        assert_eq!(
            epoch.load(Ordering::Acquire),
            0,
            "adjacent id REVIEW-12 must not arm grace for target REVIEW-1"
        );
    }

    #[test]
    fn grok_output_tail_capped_keeps_trailing_tag() {
        let mut acc = Accumulator::default();
        // Exceed 2*cap with filler, then the tag at the very end.
        let filler = "x".repeat(2 * MAX_OUTPUT_BYTES + 1024);
        accumulate(&mut acc, StreamEvent::AssistantText(filler));
        accumulate(
            &mut acc,
            StreamEvent::AssistantText("<task-status>REVIEW-1:done</task-status>".to_string()),
        );
        let output = GrokStreamFormat.derive_output(&acc);
        assert!(output.len() <= MAX_OUTPUT_BYTES);
        assert!(
            output.ends_with("<task-status>REVIEW-1:done</task-status>"),
            "tail (with the completion tag) must survive truncation"
        );
    }

    #[test]
    fn codex_agent_messages_accumulate_into_output_and_preserve_completion_tags() {
        let lines = [
            r#"{"type":"thread.started","thread_id":"abc"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"partial "}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"<task-status>CODEX-1:done</task-status>\n<completed>CODEX-1</completed>"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#,
        ];
        let (output, _conv, _d) = run_format(&CodexStreamFormat, &lines);
        assert!(output.contains("<task-status>CODEX-1:done</task-status>"));
        assert!(output.contains("<completed>CODEX-1</completed>"));
    }

    #[test]
    fn codex_turn_completed_text_is_not_authoritative_output() {
        let lines = [
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"assistant text"}}"#,
            r#"{"type":"turn.completed","text":"wrong final"}"#,
        ];
        let (output, _conv, _d) = run_format(&CodexStreamFormat, &lines);
        assert_eq!(output, "assistant text");
    }

    #[test]
    fn codex_error_events_enter_transcript_not_output() {
        let lines = [r#"{"type":"turn.failed","error":{"message":"auth failed"}}"#];
        let (output, conv, _d) = run_format(&CodexStreamFormat, &lines);
        assert_eq!(output, "");
        assert!(conv.contains("[Error: auth failed]"));
    }

    #[test]
    fn grok_multibyte_chunk_boundary_stays_valid_utf8() {
        // A multibyte codepoint emitted across the buffer-cap boundary.
        let mut acc = Accumulator::default();
        accumulate(&mut acc, StreamEvent::AssistantText("✓ done ".repeat(8)));
        let output = GrokStreamFormat.derive_output(&acc);
        assert!(output.contains('✓'));
        assert_eq!(output, "✓ done ".repeat(8));
    }

    #[test]
    fn claude_errored_assistant_suppresses_text_from_transcript() {
        // Parity: tee_assistant_text shows the text live (LiveOnlyText), but the
        // transcript records only [Error: ...] — content blocks suppressed.
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"should not appear"}]},"model":"m","error":"boom"}"#;
        let (_output, conv, _d) = run_format(&ClaudeStreamFormat, &[line]);
        assert!(
            conv.contains("[Error: boom]"),
            "error must be in transcript"
        );
        assert!(
            !conv.contains("should not appear"),
            "errored-message text must be suppressed from the transcript"
        );
    }

    #[test]
    fn claude_output_from_final_result_regardless_of_chunks() {
        let lines = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"streamed"}]}}"#,
            r#"{"type":"result","subtype":"success","result":"<completed>T-1</completed>"}"#,
        ];
        let (output, _conv, _d) = run_format(&ClaudeStreamFormat, &lines);
        assert_eq!(
            output, "<completed>T-1</completed>",
            "Claude output is the authoritative result line, not the accumulated chunks"
        );
    }
}
