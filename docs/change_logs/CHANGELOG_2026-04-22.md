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

## Dedup Dismissal Memory

**Branch**: `feat/dedup-dismissals`
**PRD**: `tasks/dedup-dismissals-prompt.md`

### What shipped

`curate dedup` now persists pairs the LLM has examined and found distinct in a
new `dedup_dismissals` table (migration v18, composite PK `(id_lo, id_hi)` with
normalization enforced at write time). Subsequent runs skip clusters whose
every C(N,2) pair is already dismissed — no LLM call, no wasted review time on
identical "no duplicates" output. Dismissals are recorded only on successful
LLM batches (skipped on `dry_run` and on LLM-error batches), and merged-pair
internal cluster relationships are excluded from dismissal accounting. A new
`--reset-dismissals` flag clears the table before the run (applies even with
`--dry-run` since it is an administrative action, not an LLM pass).

### Why it matters

- Second and subsequent `curate dedup` runs now short-circuit stable clusters,
  cutting both LLM cost and the user's review time on repeat output.
- Dismissal memory is idempotent (`INSERT OR IGNORE` on a normalized PK) and
  has no foreign-key coupling to `learnings` — retired-learning rows become
  inert rather than requiring a cascading cleanup.

### Breaking changes

None. `DedupResult.clusters_skipped` is additive with `#[serde(default = 0)]`
so older JSON consumers still parse. `--reset-dismissals` is additive.

---
