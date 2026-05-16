# src/commands/next — design notes

Subsystem narrative for `task-mgr next` (eligibility scoring, soft-dep guard,
parallel-slot group selection). The parallel-slot scheduling defenses are
documented in `src/loop_engine/CLAUDE.md` (most of the cascade-hardening
logic lives there, even though `select_parallel_group` lives here).

Token-aware matching, prefix scoping, and the AC writing convention are
all enforced by `id_body_matches_prefix` / `mentioned_fixup_prefixes` in
`selection.rs`. File-scoped don't-do-this rules (e.g. the AC tokenizer
false-positive on fully-prefixed task IDs) are migrated to `task-mgr learn`
so they surface via `recall --for-task`.

## Soft-dep guard for milestone scheduling

`build_scored_candidates` in `src/commands/next/selection.rs` applies a **soft-dep
filter** after the formal `dependsOn` check. It defers any candidate whose
acceptance criteria reference a known spawned-fixup prefix
(`SPAWNED_FIXUP_PREFIXES = ["REFACTOR-N", "CODE-FIX", "WIRE-FIX", "IMPL-FIX"]`)
while a same-prefix `todo`/`in_progress` sibling still exists in the same PRD.
Defends against forgotten `--depended-on-by <milestone-id>` edges when the loop
spawns ad-hoc fixups in response to a milestone's AC text.

**Invariants for future maintainers:**

- **Token-aware exact-prefix matching, never loose substring**:
  `id_body_matches_prefix` requires the `{prefix}-` boundary at start-of-id OR
  after a `-`. Bare `id.contains("CODE-FIX")` would false-match `CODE-FIXTURE-1`
  — that's the regression the trailing dash exists to prevent.
- **AC writing convention**: the filter tokenizes acceptance-criteria text on
  non-`[A-Z0-9-]` chars and matches `token.starts_with("{prefix}-")`. Tokens
  must start with the bare prefix — an AC that writes a fully task-prefixed form
  like `cbd7d081-REFACTOR-N-xxx` tokenizes as one token starting with
  `cbd7d081-` and **silently bypasses** the guard. PRD authors who want the
  guard to fire should write the prefix as a standalone token (`REFACTOR-N-xxx`,
  `CODE-FIX-xxx`, etc.) — typically inside a parenthetical or slash-list as in
  `"Any spawned CODE-FIX/WIRE-FIX/IMPL-FIX/REFACTOR-N tasks have passes=true"`.
- **Self-fixup short-circuit**: `task_is_self_fixup` returns early so a
  `REFACTOR-N-001` candidate whose own AC mentions `REFACTOR-N-xxx` is never
  blocked. Sibling fixups remain co-schedulable across slots — this is the
  primary reason the guard fires only on milestone-class candidates.
- **`task_prefix` threading is mandatory**: `get_active_task_ids` mirrors
  `get_completed_task_ids` exactly — `prefix_and` clause + `archived_at IS NULL`.
  Omitting either is a known regression source: the prefix scoping is the only
  defense against PRD-A's milestone being blocked by PRD-B's active fixup, and
  archived rows must never block (they're inert).
- **`SPAWNED_FIXUP_PREFIXES` is the sole expansion point**: adding a new
  ad-hoc-spawn task type (e.g. `PERF-FIX`) requires extending this slice;
  `mentioned_fixup_prefixes` and `find_active_blockers_for_prefixes` iterate
  it directly, no other registration needed.

**Operator visibility**: a single `eprintln!` per deferred candidate
(`"Deferring <id>: AC references active fixup task(s): <sorted blocker IDs>"`)
fires at the filter site — not per-blocker, not per-AC-line. Sort order in
the message is stable for grep friendliness.

**Companion prompt-side teaching** (`src/loop_engine/prompt_sections/task_ops.rs`):
the loop agent is taught to pass `--depended-on-by <milestone-id>` when spawning
a fix in response to a milestone's AC. The selection-side guard is the catch;
the prompt-side teaching is the cause-fix. Both layers ship together by design
— neither is sufficient alone.

## Cross-references

- **Parallel-slot group selection** (`select_parallel_group`,
  `IMPLICIT_OVERLAP_FILES`, `BUILDY_TASK_PREFIXES`,
  `ephemeral_overlay`) — see the "Parallel-slot scheduling" section in
  `src/loop_engine/CLAUDE.md`. The shared-infra synthetic slot, buildy
  heuristic, and cross-wave overlay all live in this directory but are
  documented in the loop_engine narrative because the cascade-hardening
  defenses are owned by the loop engine.
