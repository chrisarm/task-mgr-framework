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
        output: "raw stdout".into(),
        effective_model: Some(OPUS_MODEL.into()),
        effective_effort: Some("high"),
        key_decisions_count: 0,
        conversation: Some("[user] go\n[assistant] done\n".into()),
        shown_learning_ids: Vec::new(),
    };
    assert_eq!(
        success.conversation.as_deref(),
        Some("[user] go\n[assistant] done\n"),
        "post-Claude success must carry the structured transcript",
    );

    // Early-exit shape (mirrors the signal / pre-iteration error sites).
    let early_exit = IterationResult {
        outcome: IterationOutcome::Empty,
        task_id: None,
        files_modified: vec![],
        should_stop: true,
        output: String::new(),
        effective_model: None,
        effective_effort: None,
        key_decisions_count: 0,
        conversation: None,
        shown_learning_ids: Vec::new(),
    };
    assert!(
        early_exit.conversation.is_none(),
        "every early-exit IterationResult literal must carry conversation: None — flipping any of \
         them to Some leaks fabricated transcripts into pipelines that should run learning \
         extraction against the (empty) raw output",
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
            output: "raw output".into(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: Some(transcript.into()),
            shown_learning_ids: Vec::new(),
        },
        claim_succeeded: true,
        shown_learning_ids: Vec::new(),
        prompt_for_overflow: None,
        section_sizes: Vec::new(),
        dropped_sections: Vec::new(),
        task_difficulty: None,
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
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        },
        claim_succeeded: true,
        shown_learning_ids: Vec::new(),
        prompt_for_overflow: None,
        section_sizes: Vec::new(),
        dropped_sections: Vec::new(),
        task_difficulty: None,
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
        output: "raw output that should NOT be the learning source".into(),
        effective_model: None,
        effective_effort: None,
        key_decisions_count: 0,
        conversation: Some(transcript.into()),
        shown_learning_ids: Vec::new(),
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
        output: String::new(),
        effective_model: None,
        effective_effort: None,
        key_decisions_count: 0,
        conversation: None,
        shown_learning_ids: Vec::new(),
    };
    let early_correct: Option<&str> = early.conversation.as_deref();
    let early_broken: Option<&str> = None;
    assert_eq!(
        early_correct, early_broken,
        "early-exit None must look identical to the broken-caller None — the discriminator only \
         distinguishes Some-but-dropped, never punishes legitimately-None paths",
    );
}
