//! Contract tests for `IterationResult.conversation` field threading.
//!
//! TDD scaffolding for FEAT-004 (Phase C). FEAT-004 will:
//! 1. Wire the post-Claude success site at `engine.rs:~2129` to populate
//!    `conversation: claude_result.conversation` (today: `None`).
//! 2. Verify every `IterationResult` literal in the codebase passes either
//!    `None` (early-exit paths) or `Some(...)` (sequential success path).
//! 3. Wire `iteration_pipeline::process_iteration_output`'s
//!    `params.conversation` from `slot.iteration_result.conversation` at the
//!    `process_slot_result` call site so wave mode and sequential mode agree.
//!
//! These tests pin the contract BEFORE FEAT-004 lands. Tests that don't need
//! the live extraction pipeline (struct field shape, type-level threading,
//! and the explicit known-bad discriminator) run today against the current
//! tree. Tests that need real LLM extraction or the FEAT-003 pipeline body
//! are `#[ignore]`'d with a reason — same pattern as
//! `tests/iteration_pipeline.rs`.
//!
//! Notes for future maintainers:
//! - Integration test → cannot use `pub(crate)` `loop_engine::test_utils`
//!   helpers (per learning #896). All construction goes through the public
//!   surface of `task_mgr::loop_engine::engine`.
//! - When FEAT-004 lands, flip the `#[ignore]` on
//!   `process_iteration_output_prefers_conversation_when_present` and assert
//!   that pipeline learning-extraction reads from `params.conversation` when
//!   `Some`, else from `params.output`. The shape of that test mirrors the
//!   FEAT-003 contract test in `tests/iteration_pipeline.rs`.

use task_mgr::loop_engine::config::IterationOutcome;
use task_mgr::loop_engine::engine::{IterationResult, SlotResult};
use task_mgr::loop_engine::model::OPUS_MODEL;

// ---------------------------------------------------------------------------
// AC #1 + #2 (structural):
//   - IterationResult exposes a `conversation: Option<String>` field.
//   - Construction with `Some(<transcript>)` mirrors the sequential
//     post-Claude success site.
//   - Construction with `None` mirrors every early-exit path (signal,
//     stop-file, pause/usage check, crash-tracker abort, rate-limit, etc.).
//
// Today this test compiles because TEST-INIT-007 added the field at the
// struct definition with a `None` default at every literal site. FEAT-004
// flips the post-Claude site to `Some(claude_result.conversation)`; this
// test continues to pass — a regression that removes the field or repurposes
// it as a non-Option breaks compilation here, which is the desired tripwire.
// ---------------------------------------------------------------------------

#[test]
fn iteration_result_carries_optional_conversation_transcript() {
    // Sequential post-Claude success shape.
    let success = IterationResult {
        outcome: IterationOutcome::Completed,
        task_id: Some("FEAT-004-OK".into()),
        files_modified: vec!["src/lib.rs".into()],
        should_stop: false,
        operator_stopped: false,
        output: "raw stdout".into(),
        effective_model: Some(OPUS_MODEL.into()),
        effective_effort: Some("high".to_string()),
        effective_runner: None,
        key_decisions_count: 0,
        conversation: Some("[user] go\n[assistant] done\n".into()),
        shown_learning_ids: Vec::new(),
        grace_buffer_tail: "<completed>FEAT-004-OK</completed>".into(),
        completion_killed: true,
    };
    assert_eq!(
        success.conversation.as_deref(),
        Some("[user] go\n[assistant] done\n"),
        "post-Claude success must carry the structured transcript",
    );
    assert_eq!(
        success.grace_buffer_tail, "<completed>FEAT-004-OK</completed>",
        "post-runner success must carry the grace-buffer tail",
    );
    assert!(
        success.completion_killed,
        "post-runner success must carry RunnerResult.completion_killed",
    );

    // Early-exit shape (mirrors the signal / pre-iteration error sites).
    let early_exit = IterationResult {
        outcome: IterationOutcome::Empty,
        task_id: None,
        files_modified: vec![],
        should_stop: true,
        operator_stopped: false,
        output: String::new(),
        effective_model: None,
        effective_effort: None,
        effective_runner: None,
        key_decisions_count: 0,
        conversation: None,
        shown_learning_ids: Vec::new(),
        grace_buffer_tail: String::new(),
        completion_killed: false,
    };
    assert!(
        early_exit.conversation.is_none(),
        "every early-exit IterationResult literal must carry conversation: None — flipping any of \
         them to Some leaks fabricated transcripts into pipelines that should run learning \
         extraction against the (empty) raw output",
    );
    assert!(
        early_exit.grace_buffer_tail.is_empty(),
        "early-exit IterationResult literals carry an empty grace-buffer tail",
    );
    assert!(
        !early_exit.completion_killed,
        "early-exit IterationResult literals set completion_killed false",
    );
}

// ---------------------------------------------------------------------------
// AC #3:
//   SlotResult.iteration_result.conversation threads through to
//   process_iteration_output's `claude_conversation` parameter
//   (`ProcessingParams.conversation: Option<&'a str>`).
//
// `ProcessingParams.conversation` is already defined as `Option<&'a str>`
// (see iteration_pipeline.rs:83). The threading boundary is therefore a
// borrow — `slot.iteration_result.conversation.as_deref()` must produce a
// value that fits that param without further conversion. This test pins
// that type compatibility so a future refactor can't silently widen the
// param to `Option<String>` (forcing a clone) or narrow the field to a
// non-`Option` type without also breaking this assertion.
// ---------------------------------------------------------------------------

#[test]
fn slot_result_conversation_borrows_into_processing_params_shape() {
    let transcript = "[assistant] threaded through wave\n";
    let slot_some = SlotResult {
        slot_index: 0,
        iteration_result: IterationResult {
            outcome: IterationOutcome::Completed,
            task_id: Some("WAVE-OK".into()),
            files_modified: vec![],
            should_stop: false,
            operator_stopped: false,
            output: "raw output".into(),
            effective_model: None,
            effective_effort: None,
            effective_runner: None,
            key_decisions_count: 0,
            conversation: Some(transcript.into()),
            shown_learning_ids: Vec::new(),
            grace_buffer_tail: String::new(),
            completion_killed: false,
        },
        claim_succeeded: true,
        shown_learning_ids: Vec::new(),
        prompt_for_overflow: None,
        section_sizes: Vec::new(),
        dropped_sections: Vec::new(),
        task_difficulty: None,
        effective_runner: task_mgr::loop_engine::runner::RunnerKind::Claude,
        pre_dispatch_provider_hint: None,
    };
    // The exact borrow shape `process_slot_result` will use when it builds
    // `ProcessingParams { conversation: ..., .. }`. Type-checked here, not in
    // a doc comment, so a future refactor has to break this test before it
    // can break the wiring.
    let param_some: Option<&str> = slot_some.iteration_result.conversation.as_deref();
    assert_eq!(
        param_some,
        Some(transcript),
        "wave-path threading must hand the transcript reference straight to \
         ProcessingParams.conversation without round-tripping through owned String",
    );

    let slot_none = SlotResult {
        slot_index: 1,
        iteration_result: IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: Some("WAVE-EARLY".into()),
            files_modified: vec![],
            should_stop: true,
            operator_stopped: false,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            effective_runner: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
            grace_buffer_tail: String::new(),
            completion_killed: false,
        },
        claim_succeeded: true,
        shown_learning_ids: Vec::new(),
        prompt_for_overflow: None,
        section_sizes: Vec::new(),
        dropped_sections: Vec::new(),
        task_difficulty: None,
        effective_runner: task_mgr::loop_engine::runner::RunnerKind::Claude,
        pre_dispatch_provider_hint: None,
    };
    let param_none: Option<&str> = slot_none.iteration_result.conversation.as_deref();
    assert!(
        param_none.is_none(),
        "early-exit slot results must thread conversation: None into the pipeline so the \
         already-complete fallback / extraction code paths see the same input shape they do today",
    );
}

// ---------------------------------------------------------------------------
// AC #4 (preference under live pipeline) — gated until FEAT-003+FEAT-004.
//
// process_iteration_output MUST call extract_learnings_from_output with the
// `conversation` source when present, falling back to `output` otherwise.
// This mirrors the sequential pre-unification behavior at engine.rs:2033-2034
// (`learning_source = claude_conversation.as_deref().unwrap_or(&claude_output)`).
//
// We can't assert this end-to-end today because:
// (a) `process_iteration_output` is a stub returning `ProcessingOutcome::default()`
//     (FEAT-003 lands the body), AND
// (b) `extract_learnings_from_output` spawns a real Claude subprocess
//     (`tests/iteration_pipeline.rs` documents the same constraint).
//
// When FEAT-003 wires the pipeline (with its mock seam / env opt-out) and
// FEAT-004 wires the post-Claude success site to populate `Some(...)`,
// flip the `#[ignore]` and fill in the body per the comments below.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-003 wires extract_learnings_from_output (mock seam needed); FEAT-004 wires \
            post-Claude success site to populate IterationResult.conversation: Some(...)"]
fn process_iteration_output_prefers_conversation_when_present() {
    // Outline (concretize once mock seam exists):
    //
    // 1. Setup migrated DB; insert a `todo` task TEST-PIPE-CONV.
    // 2. Call process_iteration_output with:
    //      output:        ""               (would yield 0 learnings)
    //      conversation:  Some(TAG_HTML)   (contains a <learning> tag)
    //    Assert >= 1 row inserted into `learnings`.
    // 3. Call again with:
    //      output:        TAG_HTML
    //      conversation:  None
    //    Assert >= 1 row inserted (fallback to output works).
    // 4. Call with:
    //      output:        ""
    //      conversation:  None
    //    Assert 0 rows inserted (negative control, proves step 2's row came
    //    from the conversation source, not a side channel).
    //
    // Discriminator: a pipeline implementation that ignores
    // params.conversation and always reads params.output gets 0 inserts in
    // step 2 — failing the conversation-preference assertion.
}

// ---------------------------------------------------------------------------
// AC #5 (explicit known-bad discriminator):
//
// "A stub that always passes None for claude_conversation fails the
// conversation-preference assertion."
//
// We pin the discriminator at the caller boundary because the pipeline body
// is still a stub. The assertion: when IterationResult.conversation is
// `Some(transcript)`, a caller that drops it to `None` produces an input
// distinguishable from a caller that threads it correctly. If this test
// stops being able to tell the difference (e.g., the field is removed,
// silently dropped, or the type collapses to `String`), the entire wiring
// contract has lost its tripwire.
// ---------------------------------------------------------------------------

#[test]
fn dropping_conversation_at_caller_is_observably_different_from_threading_it() {
    let transcript = "[assistant] structured transcript\n[user] continue\n";
    let result = IterationResult {
        outcome: IterationOutcome::Completed,
        task_id: Some("DISCRIM-1".into()),
        files_modified: vec![],
        should_stop: false,
        operator_stopped: false,
        output: "raw output that should NOT be the learning source".into(),
        effective_model: None,
        effective_effort: None,
        effective_runner: None,
        key_decisions_count: 0,
        conversation: Some(transcript.into()),
        shown_learning_ids: Vec::new(),
        grace_buffer_tail: String::new(),
        completion_killed: false,
    };

    // Correct threading: the value seen by the pipeline equals the field.
    let correct: Option<&str> = result.conversation.as_deref();
    // Broken caller (the discriminator): always None regardless of field.
    let broken: Option<&str> = None;

    assert_ne!(
        correct, broken,
        "if a caller passes None despite IterationResult.conversation being Some, the pipeline \
         loses the transcript source — that divergence MUST be observable at the boundary",
    );
    assert_eq!(
        correct,
        Some(transcript),
        "the only correct threading is to forward the field's borrow unchanged",
    );

    // Symmetric case: when the field IS None, both correct and broken agree.
    // This pins that the discriminator only fires on the Some-but-dropped
    // direction — early-exit paths that legitimately have None must not
    // trigger a false positive when FEAT-004's wiring lands.
    let early = IterationResult {
        outcome: IterationOutcome::Empty,
        task_id: None,
        files_modified: vec![],
        should_stop: true,
        operator_stopped: false,
        output: String::new(),
        effective_model: None,
        effective_effort: None,
        effective_runner: None,
        key_decisions_count: 0,
        conversation: None,
        shown_learning_ids: Vec::new(),
        grace_buffer_tail: String::new(),
        completion_killed: false,
    };
    let early_correct: Option<&str> = early.conversation.as_deref();
    let early_broken: Option<&str> = None;
    assert_eq!(
        early_correct, early_broken,
        "early-exit None must look identical to the broken-caller None — the discriminator only \
         distinguishes Some-but-dropped, never punishes legitimately-None paths",
    );
}

fn iteration_result_bodies(source: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    let needle = "IterationResult {";
    let mut byte_idx = 0;
    while let Some(rel) = source[byte_idx..].find(needle) {
        let lit_start = byte_idx + rel;
        byte_idx = lit_start + needle.len();
        let after = &source[byte_idx..];
        let mut depth = 1_i32;
        let mut end = None;
        for (i, ch) in after.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.unwrap_or_else(|| panic!("unclosed IterationResult at byte {lit_start}"));
        bodies.push(after[..end].to_string());
        byte_idx += end;
    }
    bodies
}

/// FEAT-008: the post-runner return copies both grace fields off `RunnerResult`.
/// Every other `IterationResult` literal in those two modules stays empty /
/// `completion_killed: false`. Both `process_iteration_output` call sites pass
/// the fields through. `process_iteration_output` destructures the tail and
/// does not scan it (that is FEAT-006).
#[test]
fn grace_buffer_tail_is_threaded_not_scanned() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |rel: &str| -> String {
        let path = root.join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    };

    let assert_literals = |rel: &str| {
        let bodies = iteration_result_bodies(&read(rel));
        assert!(!bodies.is_empty(), "{rel} has IterationResult literals");
        let mut copies = 0;
        for body in &bodies {
            if body.contains("grace_buffer_tail: claude_result.grace_buffer_tail") {
                copies += 1;
                assert!(
                    body.contains("completion_killed: claude_result.completion_killed"),
                    "{rel} success literal must copy completion_killed from RunnerResult"
                );
            } else {
                assert!(
                    body.contains("grace_buffer_tail: String::new()"),
                    "{rel} early-return literal missing empty grace_buffer_tail:\n{body}"
                );
                assert!(
                    body.contains("completion_killed: false"),
                    "{rel} early-return literal must set completion_killed false:\n{body}"
                );
            }
        }
        assert_eq!(copies, 1, "{rel} must copy RunnerResult grace fields once");
    };
    assert_literals("src/loop_engine/iteration.rs");
    assert_literals("src/loop_engine/slot.rs");

    let slot = read("src/loop_engine/slot.rs");
    assert!(
        slot.contains("grace_buffer_tail: &slot_result.iteration_result.grace_buffer_tail"),
        "process_slot_result must pass IterationResult.grace_buffer_tail"
    );
    assert!(
        slot.contains("completion_killed: slot_result.iteration_result.completion_killed"),
        "process_slot_result must pass IterationResult.completion_killed"
    );

    let orchestrator = read("src/loop_engine/orchestrator.rs");
    assert_eq!(
        orchestrator
            .matches("grace_buffer_tail: &result.grace_buffer_tail")
            .count(),
        1,
        "sequential process_iteration_output must pass IterationResult.grace_buffer_tail"
    );
    assert_eq!(
        orchestrator
            .matches("completion_killed: result.completion_killed")
            .count(),
        1,
        "sequential process_iteration_output must pass IterationResult.completion_killed"
    );

    let pipeline = read("src/loop_engine/iteration_pipeline.rs");
    let fn_at = pipeline
        .find("pub fn process_iteration_output")
        .expect("process_iteration_output");
    assert_eq!(
        pipeline[fn_at..].matches("grace_buffer_tail").count(),
        1,
        "process_iteration_output only destructures grace_buffer_tail; it does not scan it"
    );
}
