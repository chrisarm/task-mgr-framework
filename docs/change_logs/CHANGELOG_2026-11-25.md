# Changelog — 2026-11-25

## curate enrich remembers what it has examined

**Branch**: `fix/enrich-examined-memory`
**PRD**: none (direct fix)

### What shipped

`task-mgr curate enrich` records each learning the model has answered for in a new `enrich_examined` table and skips it on later runs. `--reset-examined` clears the record to force a new pass. A marker is written only after a batch's edits succeed, so a dry run, a model error, or a failed write leaves the batch to be retried.

### Why it matters

A learning used to be re-sent on every run whenever a metadata field stayed empty, and most learnings legitimately have no error pattern. On one project that was 17,937 of 22,150 learnings, about 900 model batches per run. A second run now sends only learnings no batch has answered, and an interrupted run resumes where it stopped.

### Breaking changes

None. Schema migration v22 adds one table and applies on first open. Older binaries still open a v22 database (checked with the previously installed binary and `curate count`). Markers start empty, so the first run after upgrading still examines every candidate once.

---
