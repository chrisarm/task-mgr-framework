# Goal ledger: worktree-root

Started: 2026-10-22 Source: /prd-goal "task-mgr in /home/chris/Documents/startat0/Projects/task-mgr" — goal inferred from session context (backup repo, 2026-10-22): let task-mgr place git worktrees under a configurable root so they can live on a dedicated, un-snapshotted ZFS dataset. Today `compute_worktree_path` (src/loop_engine/worktree.rs:85) hard-codes `{repo-parent}/{repo-name}-worktrees/{branch}`; deleted worktrees (incl. `_build`/`deps`/`node_modules`/`.venv`) pinned 20–30 GB/week in daily ZFS snapshots of rpool/archlinux/home.
Worktree root: ../task-mgr-worktrees/
Pipeline overrides: none
Author-ahead: no
Shape: pending
Verification skill: .claude/skills/verify-task-mgr/SKILL.md
Sensitive paths: none

## Now

Phase: 1 worktree-root
State: paused
Next: Chris accepts/edits tasks/prd-goal-worktree-root-shape.md (+ OQ: missing root hard error vs fallback) → write Pins/Shape → A. Author
PRD / JSON / prompt: — / — / —
Architect / First-principles / Cut-adversary / JSON-review: — / — / — / —
FP decision: —
Worktree / prefix / branch: ../task-mgr-worktrees/ / — / —
Live writer id: none
Fix cycle: 0
Last event: 2026-10-22 shape v3 persisted → tasks/prd-goal-worktree-root-shape.md; paused for owner acceptance

## Pins (verbatim — law for every phase)

(pending shape acceptance)

## Simplified shape (do not re-expand)

(pending shape acceptance)

## Goal exit gates

- [ ] With the root configured, `task-mgr loop run` / `batch run` (incl. parallel slots) create worktrees under it, not under `{repo-parent}/{repo}-worktrees/`
- [ ] With nothing configured, behaviour and paths are byte-identical to today
- [ ] Cleanup never removes the configured root itself, and never touches another repo's worktrees under a shared root

## Phases

| #   | Phase         | PRD | JSON | State   | Land | Notes |
| --- | ------------- | --- | ---- | ------- | ---- | ----- |
| 1   | worktree-root | —   | —    | pending | —    | draft; shape may split/merge |

States: pending → authoring → authored → looping → reviewing →
fixing(n) → compounding → merging → merged → ticked
Also: paused | skipped (STATUS already Done)

## Residuals (human gates — not merge-blocking unless the goal says so)

| ID | What | Owner |
| --- | --- | --- |
| R1 | Create ZFS dataset (e.g. rpool/archlinux/worktrees), set the root, migrate existing `*-worktrees` (backup repo work, outside task-mgr) | Chris |

## Log

- 2026-10-22 goal opened. Session-state regenerated 19:10 (no running loops, no holds). Main checkout on `fix/ci-green`, dirty (7 modified files) — not ours; merge via a checkout we own (sharing-main.md). Local main abf301f is 1 ahead of origin/main eded39d.
- 2026-10-22 Owner context: heaviest worktree producers are restaurant_agent (Elixir + rustler NIFs; per-worktree `_build`/`deps`, NIF builds bypass global cargo target-dir redirect) and ~/Dropbox/MedWatchers/Projects/mw_support (Rust; Dropbox client no longer runs — plain dir on snapshotted /home). Forwarded to shape agent.
- 2026-10-22 Shape report v1 received (pins: user-config `worktreeRoot`, `<root>/<repo>/<branch>`, collision = hard error). Agent being resumed with late owner context (two heavy repos; Dropbox client not running); persist after v2.
- 2026-10-22 Shape OQ1 answered by operator: 291 git repos under ~/Documents/*/Projects + ~/Dropbox/*/Projects; ~95 basenames duplicated (Documents/MedWatchers-No_Sync ↔ Dropbox/MedWatchers, Documents/startat0 ↔ Dropbox/startat0), incl. mw_support, restaurant_agent, task-mgr; 3-way: mw_integrations, mw-vault, humana-reports, humanacmrreports. → plain `<root>/<repo-name>/` would collide routinely; per-repo dir must be disambiguated.
- 2026-10-22 DECIDED G-1: goal wording inferred from session context, confirmed at shape pause rather than a separate question — the shape pause is already a mandatory owner checkpoint.
