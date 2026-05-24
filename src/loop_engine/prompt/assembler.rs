//! Data-driven prompt assembler (Phase 2 / Item 3, CONTRACT-001).
//!
//! Replaces the hand-enforced "any section added to sequential must also be
//! wired into slot" discipline with a single ordered `Vec<SectionSpec>` (a
//! fn-pointer spec table, approach C) consumed by one [`assemble`] function
//! that both the sequential and slot builders call. Each path supplies its
//! OWN ordered roster; the slot roster is a set-subset of sequential's but is
//! independently ordered. See `docs/designs/prompt-assembler-contract.md`
//! §CONTRACT-001 for the authoritative contract.
//!
//! This file is the engine + types only — no real section is migrated yet
//! (FEAT-002+ migrate sections one at a time behind byte-parity tests).
//!
//! # Render phase vs. emit order
//!
//! [`assemble`] renders all [`SectionKind::Critical`] sections FIRST so it can
//! gate the total budget, then renders [`SectionKind::Trimmable`] sections in
//! roster order, fitting each into the remaining budget. Regardless of render
//! phase, sections are EMITTED in roster order — the roster is the display
//! order. Threading the per-section budget through [`SectionKind::Trimmable`]
//! (not [`PromptContext`]) keeps each trimmable's budget independent.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::commands::next::output::{LearningSummaryOutput, NextTaskOutput};
use crate::loop_engine::config::PermissionMode;
use crate::loop_engine::prompt_sections::try_fit_section;
use crate::models::Task;

/// Uniform critical-overflow signal: when criticals alone exceed
/// `total_budget`, [`assemble`] returns an empty prompt with
/// `dropped_sections == [CRITICAL_OVERFLOW_SENTINEL]`. Callers translate this
/// per their own overflow contract (the sequential caller maps it back to
/// `Err(TaskMgrError::PromptOverflow)`; the slot caller keeps its
/// sentinel-in-bundle behavior).
pub const CRITICAL_OVERFLOW_SENTINEL: &str = "CRITICAL";

/// Closed set of main-thread-available borrows handed to every render fn.
///
/// All fields are borrowed references valid only for the single [`assemble`]
/// call — render fns must not retain them. The assembler runs entirely on the
/// main thread (it borrows `&Connection`, which is `!Send`) and returns owned
/// `String`s, so the output is `Send` even though the context is not.
/// Sequential-only inputs are `Option<…>`; the slot path leaves them `None`.
pub struct PromptContext<'a> {
    pub conn: &'a Connection,
    pub task: &'a Task,
    pub task_files: &'a [String],
    pub project_root: &'a Path,
    pub base_prompt_path: &'a Path,
    pub permission_mode: &'a PermissionMode,
    pub steering_path: Option<&'a Path>,
    pub session_guidance: &'a str,
    pub run_id: Option<&'a str>,
    pub task_prefix: Option<&'a str>,
    /// Sequential-only: reorder instruction hint. Slot leaves this `None`.
    pub reorder_hint: Option<&'a str>,
    /// Sequential-only: sibling PRD paths for `build_sibling_prd_section`.
    /// Slot leaves this `None`.
    pub batch_sibling_prds: Option<&'a [PathBuf]>,
    /// Sequential-only: the resolved model id for this iteration, the input to
    /// `build_escalation_section` (the escalation policy section is omitted when
    /// the model resolves to the Opus tier). Slot leaves this `None` — wave
    /// slots drop the escalation section entirely.
    pub resolved_model: Option<&'a str>,
    /// Sequential-only: the selected task as a [`NextTaskOutput`], the source
    /// for the sequential task-envelope render (which formats it via
    /// `core::format_next_task_json` and truncates to `TASK_CONTEXT_BUDGET`).
    /// The slot path renders the envelope from [`Self::task`] + [`Self::task_files`]
    /// instead and leaves this `None`.
    pub next_task_output: Option<&'a NextTaskOutput>,
    /// Sequential-only: the UCB-recalled learnings selected by `next::next`,
    /// the source for the sequential learnings render (formatted via
    /// `prompt_sections::learnings::build_learnings_section`, recall-limit-driven
    /// rather than budget-driven). The slot path recalls its own learnings
    /// inside `core::build_learnings_block` from [`Self::conn`] + [`Self::task`]
    /// and leaves this `None`.
    pub recalled_learnings: Option<&'a [LearningSummaryOutput]>,
}

/// How a section participates in budget accounting.
///
/// Criticals render first and must collectively fit within `total_budget`
/// (overflow returns the [`CRITICAL_OVERFLOW_SENTINEL`]); they are never
/// degraded. Trimmables fill the remaining budget in roster order, carrying
/// their own per-section `budget` so independent caps (e.g. learnings vs.
/// source context) do not collapse into a single shared field.
#[derive(Clone, Copy)]
pub enum SectionKind {
    Critical,
    Trimmable { budget: usize },
}

/// Output of a single render fn: the section text plus any learning IDs the
/// section surfaced (side output, centralized so `shown_learning_ids` can be
/// cleared in exactly one place when a learnings section is dropped).
#[derive(Default)]
pub struct Rendered {
    pub text: String,
    pub shown_learning_ids: Vec<i64>,
}

/// One entry in a path's roster: a stable name, its budget kind, and the
/// render fn. The render fn is a plain `fn` pointer (no captures) so a
/// `SectionSpec` is `Send` for free — a roster may be referenced while
/// building a `SlotPromptBundle`. `Copy` lets a builder derive sub-rosters
/// (e.g. criticals-only) from a single declarative roster without re-listing
/// specs.
#[derive(Clone, Copy)]
pub struct SectionSpec {
    /// Stable id; matches the keys used in [`Assembled::section_sizes`].
    pub name: &'static str,
    pub kind: SectionKind,
    /// `kind` is passed in so a trimmable render fn can read its own budget.
    pub render: fn(&PromptContext, SectionKind) -> Rendered,
}

/// The fully assembled prompt plus accounting for observability and the
/// learnings feedback loop.
pub struct Assembled {
    pub prompt: String,
    /// `(name, emitted_byte_len)` in roster order. Dropped sections record 0.
    pub section_sizes: Vec<(&'static str, usize)>,
    /// Names of non-empty trimmables that did not fit, or the single
    /// [`CRITICAL_OVERFLOW_SENTINEL`] when criticals overflowed.
    pub dropped_sections: Vec<String>,
    /// Learning IDs from sections that actually fit (dropped sections
    /// contribute nothing, so the UCB bandit is never credited for learnings
    /// the agent never saw).
    pub shown_learning_ids: Vec<i64>,
}

impl Assembled {
    /// Borrow the rendered text of a single section by `name`, sliced out of
    /// the concatenated [`Self::prompt`] using the [`Self::section_sizes`]
    /// offsets. Returns `""` when the name is absent.
    ///
    /// Used by builders that are mid-migration: only a subset of sections is on
    /// the roster, so the concatenated `prompt` cannot be spliced into the
    /// final (interleaved) layout wholesale — each migrated section's bytes are
    /// pulled out here and re-stitched at its legacy display position alongside
    /// the still-inlined sections. Because `section_sizes` is built in lockstep
    /// with `prompt` (same roster order), the cumulative offsets land exactly on
    /// section boundaries, so the slice is always valid UTF-8. On a critical
    /// overflow `prompt` is empty while `section_sizes` is populated; `.get`
    /// then yields `""` rather than panicking.
    #[must_use]
    pub fn section_text(&self, name: &str) -> &str {
        let mut offset = 0usize;
        for (n, len) in &self.section_sizes {
            if *n == name {
                return self.prompt.get(offset..offset + len).unwrap_or("");
            }
            offset += len;
        }
        ""
    }
}

// CONTRACT-001: `SectionSpec` must be `Send` so a roster can be referenced
// while building the `Send`-safe `SlotPromptBundle`. fn-pointer specs are
// `Send` for free; this is cheap insurance against a future field that isn't.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<SectionSpec>();
};

/// Assemble a prompt from `roster` (display order) within `total_budget`.
///
/// Criticals render first to gate the budget; trimmables then fill the
/// remainder in roster order via the shared [`try_fit_section`] primitive.
/// Sections emit in roster order regardless of render phase. If criticals
/// alone exceed `total_budget`, returns an empty prompt with
/// `dropped_sections == [CRITICAL_OVERFLOW_SENTINEL]`.
pub fn assemble(ctx: &PromptContext, roster: &[SectionSpec], total_budget: usize) -> Assembled {
    // Phase 1: render criticals and gate the total budget.
    let mut rendered: Vec<Option<Rendered>> = (0..roster.len()).map(|_| None).collect();
    let mut critical_bytes: usize = 0;
    for (i, spec) in roster.iter().enumerate() {
        if matches!(spec.kind, SectionKind::Critical) {
            let r = (spec.render)(ctx, spec.kind);
            critical_bytes = critical_bytes.saturating_add(r.text.len());
            rendered[i] = Some(r);
        }
    }

    if critical_bytes > total_budget {
        // Report the rendered critical sizes (trimmables, not yet rendered,
        // record 0) so callers can attribute the overflow to specific sections
        // in diagnostics dumps — the slot builder already surfaces this same
        // breakdown in its sentinel bundle. `prompt` stays empty: an overflowed
        // critical set is never dispatched, only translated by the caller.
        let section_sizes = roster
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let size = rendered[i].as_ref().map_or(0, |r| r.text.len());
                (spec.name, size)
            })
            .collect();
        return Assembled {
            prompt: String::new(),
            section_sizes,
            dropped_sections: vec![CRITICAL_OVERFLOW_SENTINEL.to_string()],
            shown_learning_ids: Vec::new(),
        };
    }

    // Phase 2: render trimmables in roster order, fitting each into the
    // remaining budget. `try_fit_section` is the existing budget-degradation
    // primitive (learning 2663) — reused verbatim, not reimplemented.
    let mut remaining = total_budget - critical_bytes;
    let mut dropped_sections: Vec<String> = Vec::new();
    let mut dropped_flag = vec![false; roster.len()];
    for (i, spec) in roster.iter().enumerate() {
        if let SectionKind::Trimmable { .. } = spec.kind {
            let r = (spec.render)(ctx, spec.kind);
            let before = dropped_sections.len();
            let fitted = try_fit_section(r.text, spec.name, &mut remaining, &mut dropped_sections);
            dropped_flag[i] = dropped_sections.len() != before;
            rendered[i] = Some(Rendered {
                text: fitted,
                shown_learning_ids: r.shown_learning_ids,
            });
        }
    }

    // Phase 3: emit in roster order. A dropped section contributes no text and
    // — crucially — none of its `shown_learning_ids`.
    let mut prompt = String::with_capacity(total_budget);
    let mut section_sizes: Vec<(&'static str, usize)> = Vec::with_capacity(roster.len());
    let mut shown_learning_ids: Vec<i64> = Vec::new();
    for (i, spec) in roster.iter().enumerate() {
        let r = rendered[i].take().unwrap_or_default();
        prompt.push_str(&r.text);
        section_sizes.push((spec.name, r.text.len()));
        if !dropped_flag[i] {
            shown_learning_ids.extend(r.shown_learning_ids);
        }
    }

    Assembled {
        prompt,
        section_sizes,
        dropped_sections,
        shown_learning_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a throwaway `PromptContext` for tests. The synthetic render fns
    /// below ignore every field, but `assemble` needs a real context to pass
    /// through. Owned backing values are returned alongside so the borrows
    /// stay valid for the call.
    struct TestCtx {
        conn: Connection,
        task: Task,
        task_files: Vec<String>,
        root: PathBuf,
        base_prompt: PathBuf,
        mode: PermissionMode,
        guidance: String,
    }

    impl TestCtx {
        fn new() -> Self {
            TestCtx {
                conn: Connection::open_in_memory().expect("in-memory db"),
                task: Task::new("FEAT-001", "test"),
                task_files: Vec::new(),
                root: PathBuf::from("/tmp"),
                base_prompt: PathBuf::from("/tmp/prompt.md"),
                mode: PermissionMode::Dangerous,
                guidance: String::new(),
            }
        }

        fn ctx(&self) -> PromptContext<'_> {
            PromptContext {
                conn: &self.conn,
                task: &self.task,
                task_files: &self.task_files,
                project_root: &self.root,
                base_prompt_path: &self.base_prompt,
                permission_mode: &self.mode,
                steering_path: None,
                session_guidance: &self.guidance,
                run_id: None,
                task_prefix: None,
                reorder_hint: None,
                batch_sibling_prds: None,
                resolved_model: None,
                next_task_output: None,
                recalled_learnings: None,
            }
        }
    }

    fn render_a(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered {
            text: "A".to_string(),
            ..Default::default()
        }
    }
    fn render_b(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered {
            text: "B".to_string(),
            ..Default::default()
        }
    }
    fn render_c(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered {
            text: "C".to_string(),
            ..Default::default()
        }
    }
    fn render_big(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered {
            text: "x".repeat(100),
            ..Default::default()
        }
    }
    fn render_empty(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered::default()
    }
    fn render_learnings_big(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered {
            text: "L".repeat(100),
            shown_learning_ids: vec![1, 2, 3],
        }
    }
    fn render_learnings_small(_: &PromptContext, _: SectionKind) -> Rendered {
        Rendered {
            text: "L".to_string(),
            shown_learning_ids: vec![7, 8],
        }
    }

    #[test]
    fn emits_in_roster_order_not_render_phase_order() {
        // Roster interleaves criticals around a trimmable. Render phase order
        // is A, C (criticals first), then B — so a phase-order emit would
        // produce "ACB". Roster (= display) order must produce "ABC".
        let tc = TestCtx::new();
        let roster = [
            SectionSpec {
                name: "a",
                kind: SectionKind::Critical,
                render: render_a,
            },
            SectionSpec {
                name: "b",
                kind: SectionKind::Trimmable { budget: 1000 },
                render: render_b,
            },
            SectionSpec {
                name: "c",
                kind: SectionKind::Critical,
                render: render_c,
            },
        ];
        let out = assemble(&tc.ctx(), &roster, 80_000);
        assert_eq!(out.prompt, "ABC");
        assert_eq!(
            out.section_sizes,
            vec![("a", 1), ("b", 1), ("c", 1)],
            "section_sizes must also follow roster order"
        );
        assert!(out.dropped_sections.is_empty());
    }

    #[test]
    fn criticals_over_budget_yield_empty_prompt_and_critical_sentinel() {
        let tc = TestCtx::new();
        let roster = [
            SectionSpec {
                name: "a",
                kind: SectionKind::Critical,
                render: render_big,
            },
            SectionSpec {
                name: "b",
                kind: SectionKind::Critical,
                render: render_big,
            },
        ];
        // Two 100-byte criticals against a 150-byte budget overflow.
        let out = assemble(&tc.ctx(), &roster, 150);
        assert_eq!(out.prompt, "");
        assert_eq!(out.dropped_sections, vec!["CRITICAL".to_string()]);
        assert!(out.shown_learning_ids.is_empty());
        // The per-section breakdown is still reported on overflow so callers
        // can attribute the overflow (and sum a faithful `critical_size`).
        assert_eq!(
            out.section_sizes,
            vec![("a", 100), ("b", 100)],
            "critical overflow must still report rendered section sizes"
        );
        // `section_text` must not panic against the empty overflow prompt.
        assert_eq!(out.section_text("a"), "");
    }

    #[test]
    fn section_text_slices_each_section_by_name() {
        let tc = TestCtx::new();
        let roster = [
            SectionSpec {
                name: "a",
                kind: SectionKind::Critical,
                render: render_a,
            },
            SectionSpec {
                name: "b",
                kind: SectionKind::Critical,
                render: render_b,
            },
            SectionSpec {
                name: "c",
                kind: SectionKind::Critical,
                render: render_c,
            },
        ];
        let out = assemble(&tc.ctx(), &roster, 80_000);
        assert_eq!(out.prompt, "ABC");
        assert_eq!(out.section_text("a"), "A");
        assert_eq!(out.section_text("b"), "B");
        assert_eq!(out.section_text("c"), "C");
        assert_eq!(out.section_text("missing"), "");
    }

    #[test]
    fn trimmable_that_doesnt_fit_is_dropped_but_empty_render_is_not() {
        let tc = TestCtx::new();
        let roster = [
            SectionSpec {
                name: "empty",
                kind: SectionKind::Trimmable { budget: 1000 },
                render: render_empty,
            },
            SectionSpec {
                name: "toobig",
                kind: SectionKind::Trimmable { budget: 1000 },
                render: render_big,
            },
        ];
        // 100-byte trimmable against a 10-byte total budget — it cannot fit.
        let out = assemble(&tc.ctx(), &roster, 10);
        assert_eq!(out.dropped_sections, vec!["toobig".to_string()]);
        assert!(
            !out.dropped_sections.contains(&"empty".to_string()),
            "a section rendering \"\" must NOT be named in dropped_sections"
        );
        assert_eq!(out.prompt, "", "the only non-empty section was dropped");
    }

    #[test]
    fn dropped_learnings_section_clears_shown_learning_ids() {
        let tc = TestCtx::new();
        let roster = [SectionSpec {
            name: "learnings",
            kind: SectionKind::Trimmable { budget: 1000 },
            render: render_learnings_big,
        }];
        // 100-byte learnings section against a 10-byte budget — dropped.
        let out = assemble(&tc.ctx(), &roster, 10);
        assert_eq!(out.dropped_sections, vec!["learnings".to_string()]);
        assert!(
            out.shown_learning_ids.is_empty(),
            "dropped learnings must not credit the bandit"
        );
    }

    #[test]
    fn fitting_section_contributes_its_shown_learning_ids() {
        // Counterpart to the drop test: proves ids ARE collected when the
        // section fits, so the clear-on-drop above is meaningful.
        let tc = TestCtx::new();
        let roster = [SectionSpec {
            name: "learnings",
            kind: SectionKind::Trimmable { budget: 1000 },
            render: render_learnings_small,
        }];
        let out = assemble(&tc.ctx(), &roster, 80_000);
        assert_eq!(out.prompt, "L");
        assert_eq!(out.shown_learning_ids, vec![7, 8]);
        assert!(out.dropped_sections.is_empty());
    }

    #[test]
    fn empty_roster_yields_empty_prompt_without_panic() {
        let tc = TestCtx::new();
        let out = assemble(&tc.ctx(), &[], 80_000);
        assert_eq!(out.prompt, "");
        assert!(out.section_sizes.is_empty());
        assert!(out.dropped_sections.is_empty());
        assert!(out.shown_learning_ids.is_empty());
    }
}
