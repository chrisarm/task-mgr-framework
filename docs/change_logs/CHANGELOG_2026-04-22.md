# Changelog — 2026-04-22

## Recall Score Output + Learning Supersession

**Branch**: `feat/recall-scores-and-supersession`
**PRD**: `tasks/prd-recall-scores-and-supersession.md`

### What shipped

`task-mgr recall --format json` now returns numeric scores (`relevance_score`,
`ucb_score`, `combined_score`) plus the backend-authoritative `match_reason`
string alongside the existing categorical `confidence`. A new
`learning_supersessions` join table (migration v17) tracks replacement
relationships; `task-mgr learn --supersedes <id>` and `edit-learning <new-id>
--supersedes <old-id>` record them, auto-downgrade the old learning's
confidence to `low`, and `recall` auto-filters superseded rows by default
(opt back in with `--include-superseded`). `learnings list` annotates rows
with `(superseded by #N)` / `(supersedes #M)`.

### Why it matters

- Loop prompt builders and pattern-match consumers can now parse numeric
  signal strength instead of squeezing a three-bucket `confidence` field.
- Knowledge curators can replace outdated learnings without destroying
  history — the audit trail is retained in `learning_supersessions` and the
  old row still surfaces when deliberately queried.
- Graduation workflows in higher-level skills (e.g. `/compound`) can now
  express "this cluster has been superseded by a specialist agent" as
  first-class data instead of a tag convention.

### Breaking changes

None. `recall_learnings()` and `recall_learnings_with_backend()` signatures
are unchanged; scored output flows through a new `recall_learnings_scored()`.
New CLI flags are additive (`--supersedes`, `--include-superseded`). New
fields on `LearningSummary` are additive with `#[serde(default)]`.

---
