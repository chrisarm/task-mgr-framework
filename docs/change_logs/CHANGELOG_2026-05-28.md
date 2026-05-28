# Changelog — 2026-05-28

## Logging standardization (CONTRACT-LOG-001)

**Branch**: `feat/logging-standardization`
**PRD**: `tasks/logging-standardization-spike.md` (`/plan-tasks` lean flow, no standalone PRD doc)
**Prefix**: `147cf226`

### What shipped

A new four-channel logging boundary, classified per [CONTRACT-LOG-001](../../tasks/logging-standardization-spike.md):

- **Channel A / A2** — product UX + byte-locked operator contracts, via a new `ui::` surface (`src/output/ui.rs`: `emit`, `emit_err`, `emit_data`, `emit_prefixed`, `prompt`, `yellow`). Bytes + audience FD preserved; no level/timestamp ever added.
- **Channel B** — internal diagnostics, via a `tracing` subscriber (`src/observability.rs`): console layer at `WARN+` by default (raise via `TASK_MGR_LOG=...`), rolling file layer at `DEBUG+` writing to `.task-mgr/logs/task-mgr-<prefix>.<YYYY-MM-DD>.log` (prefix-suffixed exactly like `tasks/progress-<prefix>.txt`).
- **Channel C** — child-process raw stderr captured per-iteration to `.task-mgr/logs/<prefix>-<run>-<slot>-iterN-grok-stderr.log`; `GROK_TELEMETRY_TRACE_UPLOAD=0` silences xAI BatchSpanProcessor noise. Decoupled from reactions FEAT-014.

A `tests/no_raw_prints.rs` CI guard enforces the boundary going forward.

### Why it matters

Operators get a quiet console (`WARN+` default), a complete debug log per PRD effort on disk, and an unpolluted grok-stderr capture file for post-run forensics — without losing any byte-locked snapshot tests or operator-grep contracts (`lifecycle_stderr_contract.rs` passes unchanged). New tasks now route diagnostics through `tracing::warn!` and product output through `ui::*` by clear discriminator (state-change confirmation vs internal-op-failure-that-no-ops).

### Breaking changes

None on stdout for scripts/pipes. Stderr bytes are preserved on every A2 byte-locked surface (verified by the unchanged lifecycle_stderr_contract test). Console verbosity changes: routine diagnostics that previously printed unconditionally now require `TASK_MGR_LOG=debug` (or any non-default directive) to surface.

### Follow-ups

- ~36 modules remain on the `no_raw_prints` allow-list as deliberate documented exceptions (db/, learnings/, lifecycle/, plus the rest of `loop_engine/`). A "finish full-repo migration" effort would shrink the list to empty.
- Classifier-flagged surfacing of notable grok-stderr lines from the capture files into operator/learnings flow is deferred to reactions FEAT-14.

---
