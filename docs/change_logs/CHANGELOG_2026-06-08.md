# Changelog — 2026-06-08

## Write-time embedding near-duplicate guard for learning auto-extraction

**Branch**: `feat/learning-dedup-embedding-guard`
**PRD**: `tasks/learning-dedup-embedding-guard.json`

### What shipped

The loop-engine learning auto-extraction path now skips semantic near-duplicates
at **write time** instead of letting them accumulate until the post-hoc `curate
dedup` pass. A new two-tier guard sits in `extract_learnings_from_output()`:
Tier-1 (exact `outcome`+`title` match) runs first and unconditionally; Tier-2
(`NearDuplicateChecker`, embedding cosine ≥ 0.92) only augments when an
Ollama-backed embedder is available. New pure primitives `best_match` /
`find_near_duplicate` (in `src/learnings/embeddings/mod.rs`) make the similarity
decision unit-testable without Ollama.

### Why it matters

Cross-iteration learnings that restate the same lesson with slightly reworded
titles previously polluted `recall` results until a manual `curate dedup` run
merged them. Catching them at write time keeps the recall index cleaner with no
operator action, while preserving graceful degradation: when Ollama is down the
guard is bypassed and behavior is byte-identical to exact-match-only dedup.
Uncertainty always records (asymmetric-risk bias) — a leaked dupe is cheap to
clean later, but dropping a real distinct learning is unrecoverable at write
time.

### Breaking changes

None. The guard applies to ingestion auto-extraction ONLY — the human
`task-mgr learn` and `import_learnings` paths are untouched. Offline behavior is
byte-identical to before.

---
