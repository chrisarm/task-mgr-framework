//! Per-provider limit-shape record.
//!
//! Fixture tests show how a line task-mgr already parsed is classified. They
//! do not show which live stream carried a limit sentence. This module appends
//! one JSON object to `.task-mgr/logs/limit-shape-<prefix>.jsonl` so the next
//! real CLI limit answers that. It does not change which string
//! `is_rate_limited`, `is_prompt_too_long`, or `is_transient_backend` scan.
//!
//! Channel text stays off tracing. Tracing has no redactor.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::loop_engine::detection::{is_prompt_too_long, is_rate_limited, is_transient_backend};

/// Trailing bytes kept for every text field on a limit-shape line.
pub(crate) const TAIL_BYTES: usize = 4096;

/// Provenance copied onto every line. Channel text is passed separately so a
/// provider cannot write another provider's keys.
pub(crate) struct RecordMeta<'a> {
    pub db_dir: Option<&'a Path>,
    pub active_prefix: Option<&'a str>,
    pub exit_code: i32,
    pub cli_error: bool,
    pub completion_killed: bool,
}

/// Claude channels: `result.result`, the assistant `StreamEvent::Error` string,
/// and the piped stderr tail. `result_is_error` is the result line's flag.
pub(crate) fn record_claude(
    meta: &RecordMeta<'_>,
    result_is_error: bool,
    result_text: &str,
    assistant_error: Option<&str>,
    stderr: &str,
) {
    let assistant_error = assistant_error.unwrap_or("");
    if !should_record(meta, &[result_text, assistant_error, stderr]) {
        return;
    }
    write_record(
        meta,
        json!({
            "provider": "claude",
            "exit_code": meta.exit_code,
            "cli_error": meta.cli_error,
            "completion_killed": meta.completion_killed,
            "result_is_error": result_is_error,
            "result_text": tail_text(result_text),
            "assistant_error": tail_text(assistant_error),
            "stderr": tail_text(stderr),
        }),
    );
}

/// Grok channels: the stderr buffer the auth and transient sniffs already use,
/// and the assistant text (`output`).
pub(crate) fn record_grok(meta: &RecordMeta<'_>, stderr: &str, output_tail: &str) {
    if !should_record(meta, &[stderr, output_tail]) {
        return;
    }
    write_record(
        meta,
        json!({
            "provider": "grok",
            "exit_code": meta.exit_code,
            "cli_error": meta.cli_error,
            "completion_killed": meta.completion_killed,
            "stderr": tail_text(stderr),
            "output_tail": tail_text(output_tail),
        }),
    );
}

/// Codex channels: the `turn.failed` / `error` string (not `derive_output`),
/// the stderr buffer the transient check uses, and the assistant text.
pub(crate) fn record_codex(
    meta: &RecordMeta<'_>,
    error_text: Option<&str>,
    stderr: &str,
    output_tail: &str,
) {
    let error_text = error_text.unwrap_or("");
    if !should_record(meta, &[error_text, stderr, output_tail]) {
        return;
    }
    write_record(
        meta,
        json!({
            "provider": "codex",
            "exit_code": meta.exit_code,
            "cli_error": meta.cli_error,
            "completion_killed": meta.completion_killed,
            "error_text": tail_text(error_text),
            "stderr": tail_text(stderr),
            "output_tail": tail_text(output_tail),
        }),
    );
}

/// Trailing `TAIL_BYTES` of `s`, cut on a char boundary so the field stays
/// valid UTF-8 and never grows past the cap.
pub(crate) fn tail_text(s: &str) -> String {
    if s.len() <= TAIL_BYTES {
        return s.to_string();
    }
    let mut start = s.len() - TAIL_BYTES;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

fn should_record(meta: &RecordMeta<'_>, channels: &[&str]) -> bool {
    // `completion_killed` does not suppress the record. A grace kill is still
    // exit ≠ 0; the line carries the flag so a later reader can tell them apart.
    meta.exit_code != 0 || meta.cli_error || channels.iter().copied().any(channel_matches)
}

fn channel_matches(text: &str) -> bool {
    is_rate_limited(text) || is_prompt_too_long(text) || is_transient_backend(text)
}

fn write_record(meta: &RecordMeta<'_>, value: Value) {
    let Some(dir) = meta.db_dir else {
        return;
    };
    let Some(prefix) = meta.active_prefix.map(str::trim).filter(|p| !p.is_empty()) else {
        return;
    };
    let Some(path) = limit_shape_path(dir, prefix) else {
        return;
    };
    append_json_line(&path, &value);
}

fn limit_shape_path(db_dir: &Path, prefix: &str) -> Option<PathBuf> {
    if !db_dir.is_dir() {
        return None;
    }
    let safe = sanitize_prefix(prefix);
    if safe.is_empty() {
        return None;
    }
    Some(
        db_dir
            .join("logs")
            .join(format!("limit-shape-{safe}.jsonl")),
    )
}

fn sanitize_prefix(prefix: &str) -> String {
    prefix
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

fn append_json_line(path: &Path, value: &Value) {
    static APPEND_LOCK: Mutex<()> = Mutex::new(());
    let _guard = APPEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn_not_written(path, &e);
        return;
    }
    let mut line = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(e) => {
            warn_not_written(path, &e);
            return;
        }
    };
    line.push(b'\n');
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(&line) {
                warn_not_written(path, &e);
            }
        }
        Err(e) => warn_not_written(path, &e),
    }
}

fn warn_not_written(path: &Path, err: &dyn std::fmt::Display) {
    // Path and error only. The JSON body is the channel text.
    tracing::warn!(
        target: "task_mgr::limit_shape",
        path = %path.display(),
        error = %err,
        "limit-shape record was not written"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TaskMgrError;
    use crate::loop_engine::config::{CrashType, IterationOutcome, PermissionMode};
    use crate::loop_engine::detection::{OutputSignals, analyze_output};
    use crate::loop_engine::runner::{RunnerKind, RunnerOpts, dispatch};
    use crate::loop_engine::test_utils::{
        CLAUDE_BINARY_MUTEX, CODEX_BINARY_MUTEX, EnvGuard, GROK_BINARY_MUTEX,
    };
    use std::sync::Arc;

    const PREFIX: &str = "fe92ec5b";

    fn meta(dir: &Path, exit_code: i32, cli_error: bool) -> RecordMeta<'_> {
        RecordMeta {
            db_dir: Some(dir),
            active_prefix: Some(PREFIX),
            exit_code,
            cli_error,
            completion_killed: false,
        }
    }

    fn shape_path(dir: &Path) -> PathBuf {
        dir.join("logs").join(format!("limit-shape-{PREFIX}.jsonl"))
    }

    fn read_lines(dir: &Path) -> Vec<Value> {
        let path = shape_path(dir);
        if !path.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("limit-shape line is JSON"))
            .collect()
    }

    fn assert_exact_keys(value: &Value, keys: &[&str]) {
        let obj = value.as_object().expect("json object");
        let mut actual: Vec<&str> = obj.keys().map(String::as_str).collect();
        actual.sort_unstable();
        let mut expected = keys.to_vec();
        expected.sort_unstable();
        assert_eq!(actual, expected, "keys {actual:?}");
    }

    #[test]
    fn clean_exit_without_classifier_match_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let m = meta(dir.path(), 0, false);
        record_claude(&m, false, "Finished the edit.", None, "routine diagnostic");
        record_grok(&m, "routine diagnostic", "assistant text");
        record_codex(&m, None, "routine diagnostic", "assistant text");
        assert!(
            !shape_path(dir.path()).exists(),
            "clean exit 0 with no match must not create the jsonl"
        );
    }

    #[test]
    fn nonzero_or_cli_error_appends_one_object() {
        let dir = tempfile::tempdir().unwrap();
        record_grok(
            &meta(dir.path(), 1, false),
            "no match here",
            "still no match",
        );
        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["exit_code"], 1);
        assert_eq!(lines[0]["cli_error"], false);
        assert_exact_keys(
            &lines[0],
            &[
                "provider",
                "exit_code",
                "cli_error",
                "completion_killed",
                "stderr",
                "output_tail",
            ],
        );

        let dir = tempfile::tempdir().unwrap();
        record_codex(
            &meta(dir.path(), 0, true),
            Some("cli said no"),
            "",
            "assistant text",
        );
        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["cli_error"], true);
        assert_eq!(lines[0]["error_text"], "cli said no");
        assert_exact_keys(
            &lines[0],
            &[
                "provider",
                "exit_code",
                "cli_error",
                "completion_killed",
                "error_text",
                "stderr",
                "output_tail",
            ],
        );
    }

    #[test]
    fn exit_zero_channel_match_appends_provider_keys_only() {
        let dir = tempfile::tempdir().unwrap();
        record_claude(
            &meta(dir.path(), 0, false),
            false,
            "Finished the edit and left a note.",
            Some("assistant-channel-error-distinct"),
            "You've hit your session limit",
        );
        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["provider"], "claude");
        assert_eq!(lines[0]["result_is_error"], false);
        assert_eq!(
            lines[0]["result_text"],
            "Finished the edit and left a note."
        );
        assert_eq!(
            lines[0]["assistant_error"],
            "assistant-channel-error-distinct"
        );
        assert_eq!(lines[0]["stderr"], "You've hit your session limit");
        assert_exact_keys(
            &lines[0],
            &[
                "provider",
                "exit_code",
                "cli_error",
                "completion_killed",
                "result_is_error",
                "result_text",
                "assistant_error",
                "stderr",
            ],
        );
    }

    #[test]
    fn grace_kill_nonzero_exit_still_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = meta(dir.path(), 143, false);
        m.completion_killed = true;
        record_claude(&m, false, "grace expired", None, "");
        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["completion_killed"], true);
        assert_eq!(lines[0]["exit_code"], 143);
    }

    #[test]
    fn text_fields_store_trailing_4096_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("START{}TAILMARK", "x".repeat(8000));
        record_claude(
            &meta(dir.path(), 1, false),
            false,
            &long,
            Some(&long),
            &long,
        );
        let line = &read_lines(dir.path())[0];
        for key in ["result_text", "assistant_error", "stderr"] {
            let value = line[key].as_str().unwrap();
            assert!(value.len() <= TAIL_BYTES, "{key} is {} bytes", value.len());
            assert!(value.ends_with("TAILMARK"), "{key} dropped the tail");
            assert!(!value.contains("START"), "{key} kept the head");
        }
    }

    #[test]
    fn record_does_not_trace_channel_text() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = "You've hit your session limit";
        let capture = LeakCapture::new();
        tracing::subscriber::with_default(capture.clone(), || {
            tracing::callsite::rebuild_interest_cache();
            record_claude(
                &meta(dir.path(), 0, false),
                false,
                "Finished the edit and left a note.",
                Some("assistant-channel-error-distinct"),
                sentinel,
            );
        });
        assert_eq!(read_lines(dir.path()).len(), 1);
        for hit in capture.snapshot() {
            assert!(
                !hit.contains(sentinel),
                "tracing leaked channel text: {hit}"
            );
            assert!(
                !hit.contains("assistant-channel-error-distinct"),
                "tracing leaked assistant_error: {hit}"
            );
            assert!(
                !hit.contains("Finished the edit and left a note."),
                "tracing leaked result_text: {hit}"
            );
        }
    }

    #[test]
    fn claude_stderr_limit_is_recorded_separately_and_does_not_change_outcome() {
        let _lock = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "claude-limit-stub.sh",
            r#"#!/bin/sh
echo "You've hit your session limit" >&2
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"live-only"}]},"error":"assistant-channel-error-distinct"}'
printf '%s\n' '{"type":"result","is_error":false,"result":"Finished the edit and left a note."}'
exit 1
"#,
        );
        let _env = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let capture = LeakCapture::new();
        let result = tracing::subscriber::with_default(capture.clone(), || {
            tracing::callsite::rebuild_interest_cache();
            dispatch(
                RunnerKind::Claude,
                "prompt-body",
                &PermissionMode::Dangerous,
                RunnerOpts {
                    stream_json: true,
                    db_dir: Some(dir.path()),
                    active_prefix: Some(PREFIX),
                    ..RunnerOpts::default()
                },
            )
        })
        .expect("claude stub spawn");

        assert_eq!(result.exit_code, 1);
        assert!(result.cli_error);
        assert_eq!(result.output, "Finished the edit and left a note.");
        assert_eq!(
            result.error_text.as_deref(),
            Some("assistant-channel-error-distinct")
        );
        assert!(is_rate_limited("You've hit your session limit"));
        let outcome = analyze_output(
            &result.output,
            result.exit_code,
            &OutputSignals {
                cli_error: result.cli_error,
                completion_killed: result.completion_killed,
                error_text: result.error_text.clone(),
                task_id: None,
                run_id: None,
            },
            dir.path(),
        );
        assert_ne!(outcome, IterationOutcome::RateLimit);
        assert_eq!(outcome, IterationOutcome::Crash(CrashType::RuntimeError));

        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["provider"], "claude");
        assert_eq!(lines[0]["result_is_error"], false);
        assert_eq!(
            lines[0]["result_text"],
            "Finished the edit and left a note."
        );
        assert_eq!(
            lines[0]["assistant_error"],
            "assistant-channel-error-distinct"
        );
        let stderr = lines[0]["stderr"].as_str().unwrap();
        assert!(
            stderr.contains("You've hit your session limit"),
            "stderr field: {stderr:?}"
        );
        assert!(!stderr.contains("Finished the edit"));
        assert!(
            !lines[0]["result_text"]
                .as_str()
                .unwrap()
                .contains("session limit")
        );
        assert_exact_keys(
            &lines[0],
            &[
                "provider",
                "exit_code",
                "cli_error",
                "completion_killed",
                "result_is_error",
                "result_text",
                "assistant_error",
                "stderr",
            ],
        );
        for hit in capture.snapshot() {
            assert!(!hit.contains("You've hit your session limit"), "{hit}");
            assert!(!hit.contains("assistant-channel-error-distinct"), "{hit}");
            assert!(!hit.contains("Finished the edit and left a note."), "{hit}");
        }
    }

    #[test]
    fn claude_clean_exit_writes_nothing() {
        let _lock = CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "claude-clean-stub.sh",
            r#"#!/bin/sh
echo "routine diagnostic" >&2
printf '%s\n' '{"type":"result","is_error":false,"result":"Finished the edit and left a note."}'
exit 0
"#,
        );
        let _env = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let result = dispatch(
            RunnerKind::Claude,
            "prompt-body",
            &PermissionMode::Dangerous,
            RunnerOpts {
                stream_json: true,
                db_dir: Some(dir.path()),
                active_prefix: Some(PREFIX),
                ..RunnerOpts::default()
            },
        )
        .expect("clean claude stub");
        assert_eq!(result.exit_code, 0);
        assert!(!result.cli_error);
        assert!(
            !shape_path(dir.path()).exists(),
            "clean exit must not write limit-shape"
        );
    }

    #[test]
    fn grok_stderr_502_stays_transient_and_is_recorded() {
        let _lock = GROK_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "grok-502-stub.sh",
            "#!/bin/sh\necho \"HTTP 502 Bad Gateway\" >&2\necho \"assistant kept going\"\nexit 1\n",
        );
        let _env = EnvGuard::set("GROK_BINARY", script.to_str().unwrap());
        let capture = LeakCapture::new();
        let err = tracing::subscriber::with_default(capture.clone(), || {
            tracing::callsite::rebuild_interest_cache();
            dispatch(
                RunnerKind::Grok,
                "prompt-body",
                &PermissionMode::Dangerous,
                RunnerOpts {
                    stream_json: false,
                    db_dir: Some(dir.path()),
                    active_prefix: Some(PREFIX),
                    ..RunnerOpts::default()
                },
            )
        })
        .expect_err("502 stderr is TransientBackend");
        assert!(
            matches!(
                err,
                TaskMgrError::TransientBackend {
                    retry_after_secs: None
                }
            ),
            "got {err:?}"
        );
        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["provider"], "grok");
        assert_eq!(lines[0]["cli_error"], false);
        assert_eq!(lines[0]["exit_code"], 1);
        assert_eq!(lines[0]["stderr"], "HTTP 502 Bad Gateway\n");
        assert!(
            lines[0]["output_tail"]
                .as_str()
                .unwrap()
                .contains("assistant kept going")
        );
        assert_exact_keys(
            &lines[0],
            &[
                "provider",
                "exit_code",
                "cli_error",
                "completion_killed",
                "stderr",
                "output_tail",
            ],
        );
        for hit in capture.snapshot() {
            assert!(!hit.contains("HTTP 502 Bad Gateway"), "{hit}");
            assert!(!hit.contains("assistant kept going"), "{hit}");
        }
    }

    #[test]
    fn codex_stderr_502_stays_transient_and_error_text_is_not_derive_output() {
        let _lock = CODEX_BINARY_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "codex-502-stub.sh",
            r#"#!/bin/sh
echo "HTTP 502 Bad Gateway" >&2
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"derive-output-sentence"}}'
printf '%s\n' '{"type":"turn.failed","error":{"message":"unrelated turn failure"}}'
exit 1
"#,
        );
        let _env = EnvGuard::set("CODEX_BINARY", script.to_str().unwrap());
        let err = dispatch(
            RunnerKind::Codex,
            "prompt-body",
            &PermissionMode::Dangerous,
            RunnerOpts {
                stream_json: true,
                db_dir: Some(dir.path()),
                active_prefix: Some(PREFIX),
                ..RunnerOpts::default()
            },
        )
        .expect_err("502 stderr is TransientBackend");
        assert!(
            matches!(
                err,
                TaskMgrError::TransientBackend {
                    retry_after_secs: None
                }
            ),
            "got {err:?}"
        );
        let lines = read_lines(dir.path());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["provider"], "codex");
        assert_eq!(lines[0]["error_text"], "unrelated turn failure");
        assert_eq!(lines[0]["output_tail"], "derive-output-sentence");
        assert_ne!(lines[0]["error_text"], lines[0]["output_tail"]);
        assert_eq!(lines[0]["stderr"], "HTTP 502 Bad Gateway\n");
        assert!(
            !lines[0]["stderr"]
                .as_str()
                .unwrap()
                .contains("Codex stderr:")
        );
        assert_exact_keys(
            &lines[0],
            &[
                "provider",
                "exit_code",
                "cli_error",
                "completion_killed",
                "error_text",
                "stderr",
                "output_tail",
            ],
        );
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[derive(Clone)]
    struct LeakCapture {
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl LeakCapture {
        fn new() -> Self {
            Self {
                hits: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn snapshot(&self) -> Vec<String> {
            self.hits.lock().expect("leak lock").clone()
        }
    }

    struct FieldDump(Vec<String>);

    impl tracing::field::Visit for FieldDump {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push(format!("{}={value}", field.name()));
        }
    }

    impl tracing::Subscriber for LeakCapture {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut dump = FieldDump(Vec::new());
            event.record(&mut dump);
            let rendered = format!(
                "{} {} {}",
                event.metadata().target(),
                event.metadata().name(),
                dump.0.join(" ")
            );
            self.hits.lock().expect("leak lock").push(rendered);
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }
}
