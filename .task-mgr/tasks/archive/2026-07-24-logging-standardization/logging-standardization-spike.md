# SPIKE: Logging standardization (Phase B)

## [2026-05-27] - SPIKE
**Hypothesis:** task-mgr's ~1040 `eprintln!`/`println!` call sites can be standardized
into a clean 3-stream split — product output on **stdout**, task-mgr diagnostics on
**stderr via a `tracing` subscriber**, child-agent raw stderr to a **per-iteration file** —
and the split is a file-descriptor routing decision.

**Cheapest falsifier:** Instead of building a `tracing` subscriber, read the two artifacts
that would break first: the product-output primitive (`emit_prefixed_lines`) and the
byte-locked operator-contract test (`lifecycle_stderr_contract.rs`). If product output and
diagnostics already share a file descriptor, or if any "diagnostic" is a byte-locked operator
contract, the FD-split framing is wrong and the real contract is a *classification rule*, not a
routing rule.

**Result (framing FALSIFIED):**
- `emit_prefixed_lines` (`src/loop_engine/claude.rs:240`) — the live slot-prefixed agent-text
  primitive, the most product-facing UX surface — writes to **stderr**, locking the stderr
  buffer per line. 14 call sites (claude/slot/runner/stream).
- `lifecycle_stderr_contract.rs` locks the **exact bytes** of `engine.rs:4781` PRD-sync warning
  via `libc::dup2` FD-2 capture (`harness = false`), because *"operators grep stderr for this
  warning."* Learnings #3416/#3435/#3456 confirm multiple such byte-locked stderr contracts.
- Learning #3295: libtest's default harness intercepts `eprintln!` via thread-local
  `OUTPUT_CAPTURE`; a `tracing` writer to `io::stderr()` **bypasses** it — so naively converting
  `eprintln!`→`tracing::warn!` silently changes in-process test capture behavior.
- No `tracing`/`log`/`EnvFilter`/`RUST_LOG`/`TASK_MGR_LOG` anywhere (greenfield framework).
  A small `src/output/mod.rs` already exists (`warn`/`format_warn`/`should_color` with
  `NO_COLOR`+TTY discipline) but only 3 call sites adopt it.
- Volume confirmed: **511 `eprintln!`** (plus pure `println!`), concentrated in
  `loop_engine/orchestrator.rs` (104), `worktree.rs` (36), `iteration.rs` (26),
  `wave_scheduler.rs` (24), `prd_reconcile.rs` (24), `batch.rs` (23), `curate/mod.rs` (19).

**Conclusion:** The FD is a property of *audience-channel* (machine-readable data → stdout for
`list`/`show`/`export`; human progress → stderr for loop banners + agent text), **preserved** by
the UX helpers — it is **not** the discriminator between "product output" and "diagnostic." The
discriminator is *"is this line part of the product's operator-facing surface (UX or a
documented/grepped/byte-locked contract)?"* The boundary is a **classification rule**, and that
rule is the foundational abstraction every per-module migration task depends on.

**Decision: B — emit `CONTRACT-LOG-001`** (logging boundary contract). The ~1040-site migration
is 6+ module-scoped tasks that would each invent their own A/B boundary and drift; the contract
makes the boundary single-homed and testable. Hand off to `/plan-tasks` to generate the logging
PRD with the per-module FEATs wired `dependsOn: ["CONTRACT-LOG-001"]`.

**CONTRACT emitted:** CONTRACT-LOG-001 (JSON below — paste after `/plan-tasks` stands up the PRD).

**Key learning:** task-mgr logging is not an FD-routing problem; product UX, byte-locked operator
contracts, and internal diagnostics all share stderr today. The migration is *primarily a re-home
into a `ui::` channel* (preserving bytes + FD), with only the genuine-diagnostic minority becoming
`tracing` events. Mechanical `s/eprintln!/tracing::/` is wrong and breaks operator grep + snapshot
tests.

---

## Refined stream model (the contract's core)

Four channels, not three. The first two stay **out of `tracing`**:

| # | Channel | What | Current sink | Target sink | In tracing? |
|---|---------|------|--------------|-------------|-------------|
| **A** | Product-UX | slot-prefixed agent text, iteration banners, progress headers, `list`/`show`/`stats`/`export` data | stdout *or* stderr (per audience) | same FD, routed through `ui::` | **No** |
| **A2** | Operator-contract | byte-locked / grepped messages (lifecycle PRD-sync warnings, overflow announcements operators watch) | stderr (byte-exact) | stderr (byte-exact) via `ui::`, snapshot tests keep dup2 capture | **No** |
| **B** | task-mgr diagnostics | debug/trace/info/warn that nobody snapshot-tests and isn't a UX surface | stderr (`eprintln!`) | `tracing`: console at WARN+ (`TASK_MGR_LOG`), DEBUG+ → `.task-mgr/logs/task-mgr.log` | **Yes** |
| **C** | child-agent raw stderr | grok internal tracing / xAI 502 Cloudflare HTML / `BatchSpanProcessor.ExportError` | console (teed every line, `runner.rs:1037`) | per-**slot**-per-iteration file; console only when classifier (FEAT-014) flags | n/a (raw passthrough) |

**Discriminator decision tree** (applied at every `eprintln!`/`println!` during migration):

```
Is the exact byte text asserted by a test, documented for operators to grep,
or a deliberate human-facing status/banner/summary/CLI-data line?
├─ YES → channel A / A2.  Route through ui::emit{,_err,_data}. Preserve bytes + FD.
│        NEVER decorate with level/timestamp. NEVER move to tracing.
└─ NO  → channel B.  It's an internal diagnostic. Convert to tracing::{trace,debug,info,warn,error}!
         Default level = the noise floor it deserves (most current "FYI" eprintln! → debug!).
```

Stream **C** is a separate, mechanical change at the grok sniffer + `apply_common_env`
(see Coordination below) and is independent of the A/B classification.

---

## Approaches & tradeoffs

| Approach | What | Pros | Cons | Verdict |
|---|---|---|---|---|
| **1. `tracing` + `tracing-appender` + `ui::` channel (chosen)** | New `src/observability.rs` inits a `tracing_subscriber::registry()` with an `EnvFilter` (env `TASK_MGR_LOG`, console layer at WARN+) + a `tracing_appender` rolling-daily file layer at DEBUG+ to `.task-mgr/logs/task-mgr.log`. `src/output/` grows into the `ui::` product channel (A/A2). Migration classifies each site into A/A2 (→`ui::`) or B (→`tracing`). | Standard, structured, level/target/span filtering; file appender for post-mortem; `ui::` keeps product surface centralized + testable; coexists with byte-locked contracts because A2 never enters tracing. | Two deps; subscriber-vs-`eprintln!` test-capture divergence (mitigated: A2 keeps `ui::` + dup2 tests; B is debug-only off the console by default). | **Adopt.** |
| **2. `log` + `env_logger`** | Classic facade. | Lighter; familiar. | No spans (loop has rich slot/iteration/run context worth structured fields); weaker file-rotation story; still needs the A/B classification (the hard part is identical). | Reject — the classification is the real work; spans add real value for the wave/slot model. |
| **3. Hand-rolled `ui::` + `diag::` macros, no crate** | Two in-house macro families writing to FD + file. | Zero deps; total control of bytes. | Reinvents level filtering, env config, rotation, span context; more code to maintain than the migration itself; the thing we'd build *is* a worse `tracing`. | Reject. |

All three require the same A/B classification — that's why the **contract**, not the crate choice,
is the spike's deliverable.

## Residual risks (post-decision) + inversion

1. **Mis-classification buries product UX in a log file (or decorates a contract line).**
   *Inversion — how to guarantee failure:* let each module's migration author guess the A/B
   boundary. *Guard:* the contract's decision tree + a test that the byte-locked contracts
   (`lifecycle_stderr_contract` et al.) still pass unchanged after each module migrates, and a
   grep/clippy guard (below) that fails CI if a new raw `eprintln!`/`println!` appears in an
   already-migrated module.
2. **Console/file interleaving under parallel-wave threads.** Multiple slot threads + the
   tracing console layer all target stderr. *Mitigation:* both `emit_prefixed_lines` and the
   tracing `fmt` layer are **line-atomic** (lock + whole-line write), so interleaving is at line
   granularity (acceptable, already true today). Stream **C** files must be keyed
   **per-slot-per-iteration** (not just per-iteration) so concurrent grok children don't
   interleave in one file.

---

## CONTRACT-LOG-001 (ready to paste after `/plan-tasks`)

> Do **not** `task-mgr add` this into the currently-active reactions PRD (prefix `31342c85`).
> Run `/plan-tasks "logging standardization"` first to stand up the logging PRD, then paste.

```json
{
  "id": "CONTRACT-LOG-001",
  "title": "Logging boundary contract: ui:: product channel vs tracing diagnostics vs child-stderr capture",
  "taskType": "contract",
  "description": "Design-only. Define the single classification rule + module surface that every per-module print-migration FEAT implements against, so 6+ parallel migrations cannot drift on the product-vs-diagnostic boundary. NOT a file-descriptor split: product UX (emit_prefixed_lines), byte-locked operator contracts (lifecycle_stderr_contract), and internal diagnostics all share stderr today. The contract specifies: (A/A2) a ui:: channel in src/output/ that preserves exact bytes + audience FD and NEVER decorates with level/timestamp, with ui::emit (human stderr progress), ui::emit_err, ui::emit_data (stdout machine/CLI data), and ui::emit_prefixed (re-home of emit_prefixed_lines); (B) a tracing subscriber in a new src/observability.rs initialized once from main.rs, EnvFilter via TASK_MGR_LOG, console layer WARN+, tracing-appender rolling-daily file layer DEBUG+ to .task-mgr/logs/task-mgr.log; (C) child-agent raw stderr captured to a per-slot-per-iteration file under .task-mgr/logs/, surfaced to console only when the FEAT-014 classifier flags it, with GROK_TELEMETRY_TRACE_UPLOAD=0 set on the grok child in apply_common_env. The contract owns the discriminator decision tree, the byte-preservation invariant for A2, the test-capture rule (A2 keeps ui:: + dup2 snapshot tests; B is console-suppressed by default so OUTPUT_CAPTURE divergence is moot), and the .gitignore managed-block addition for .task-mgr/logs/.",
  "acceptanceCriteria": [
    "ui:: channel surface is specified in src/output/: signatures for emit (human progress, stderr), emit_err, emit_data (machine/CLI data, stdout), emit_prefixed (re-home of emit_prefixed_lines incl. the empty-text + interior-blank-line semantics) — each documents which FD it targets and that it never adds level/timestamp",
    "Discriminator decision tree is written verbatim: a call site is channel A/A2 iff its exact bytes are asserted by a test, documented for operators to grep, or a deliberate human-facing banner/summary/CLI-data line; otherwise it is channel B (tracing)",
    "Byte-preservation invariant for A2: every message currently locked by a snapshot test (lifecycle_stderr_contract.rs and any sibling) MUST emit byte-identical output after migration; those tests pass unchanged",
    "tracing init contract: src/observability.rs::init() is idempotent, called once from main.rs before any loop work, EnvFilter reads TASK_MGR_LOG (default: console WARN+), file layer is tracing-appender daily-rolling at DEBUG+ to .task-mgr/logs/task-mgr.log; init failure degrades to console-only and never aborts the CLI",
    "Test-capture rule documented: channel B events are console-suppressed by default (WARN+ only) so they do not pollute test output, and A2 retains dup2/harness=false capture — no in-process test relies on OUTPUT_CAPTURE of a migrated B-site",
    "Stream C contract: spawn_grok_stderr_sniffer writes child stderr to a per-slot-per-iteration file under .task-mgr/logs/ (path scheme specified, slot-keyed to avoid concurrent interleave) instead of teeing every line to console; lines surface to console only when the classifier flags them; GROK_TELEMETRY_TRACE_UPLOAD=0 is set in apply_common_env",
    "Known-bad: a naive migration that runs s/eprintln!/tracing::warn!/ would (a) decorate byte-locked A2 lines with a WARN prefix → breaks lifecycle_stderr_contract + operator grep, and (b) move product banners off the console into a DEBUG file nobody watches — the contract names both failure shapes",
    "Downstream impact list enumerated: MIGRATE-ORCHESTRATOR, MIGRATE-WORKTREE, MIGRATE-ITERATION, MIGRATE-WAVE-SCHEDULER, MIGRATE-PRD-RECONCILE, MIGRATE-BATCH, MIGRATE-CURATE, plus OBSERVABILITY-INIT (src/observability.rs) and STREAM-C-CAPTURE (grok sniffer) all dependOn this contract",
    "CI guard specified: a grep/clippy lint fails the build if a new raw eprintln!/println! appears in any module already marked migrated (allow-list shrinks per migration FEAT)",
    "Coordination note with reactions PRD (31342c85) recorded: FEAT-014 only READS the existing stderr sniff buffer; stream-C file capture changes only the console-tee at runner.rs:1037 — orthogonal, either lands first, trivial rebase",
    "Full contract text recorded in the logging PRD progress log under ## CONTRACT-LOG-001"
  ],
  "priority": 1,
  "estimatedEffort": "medium",
  "touchesFiles": ["src/output/mod.rs", "src/observability.rs", "src/main.rs", "Cargo.toml"],
  "dependsOn": [],
  "modifiesBehavior": false,
  "qualityDimensions": [
    "The boundary must be stable enough that 7 independent module migrations implement against it without revision",
    "A2 byte-preservation is non-negotiable — operators grep these lines and snapshot tests lock them",
    "Stream C is orthogonal to FEAT-014; the contract must not couple the logging effort to the reactions effort's merge order"
  ],
  "notes": "Produced by /spike on 2026-05-27. Falsified the FD-split framing: emit_prefixed_lines writes to stderr (claude.rs:240); lifecycle_stderr_contract.rs byte-locks stderr via libc::dup2 (harness=false) because operators grep it; learning #3295 = libtest OUTPUT_CAPTURE intercepts eprintln! (a tracing writer bypasses it). 511 eprintln! concentrated in orchestrator.rs(104)/worktree(36)/iteration(26)/wave_scheduler(24)/prd_reconcile(24)/batch(23)/curate(19). Migration is mostly a re-home into ui:: (preserving bytes+FD); only the genuine-diagnostic minority becomes tracing. Out of scope: FEAT-014's classification/reaction logic and grok's [toolset.bash] timeout_secs."
}
```

## Coordination with the Reactions Framework (prefix 31342c85)

Both efforts touch `src/loop_engine/runner.rs`:
- **Reactions / FEAT-014** only *reads* the existing `stderr_buf` sniff buffer
  (`spawn_grok_stderr_sniffer` already returns `Arc<Mutex<String>>`) to classify a transient 502.
- **Logging / STREAM-C-CAPTURE** changes only the `emit_prefixed_lines(label, &line)` **console
  tee** at `runner.rs:1037` → per-slot-per-iteration file, and sets `GROK_TELEMETRY_TRACE_UPLOAD=0`
  in `apply_common_env`.

These are orthogonal at the function: the sniff buffer that FEAT-014 reads is untouched by the
logging change; the console-tee that logging changes is unread by FEAT-014. Whichever merges
first, the other rebases trivially. **No sequencing dependency** between the two PRDs.

## Migration phasing (for `/plan-tasks`)

1. `CONTRACT-LOG-001` (this) → 2. `OBSERVABILITY-INIT` (subscriber + appender, `ui::` skeleton,
   gitignore `.task-mgr/logs/`) → 3. per-module `MIGRATE-*` FEATs, biggest-first
   (orchestrator → worktree → iteration → wave_scheduler → prd_reconcile → batch → curate),
   each: classify sites A/A2/B, route, keep byte-locked tests green, shrink the CI allow-list →
   4. `STREAM-C-CAPTURE` (grok sniffer → file + telemetry-off) → 5. `REVIEW-001` milestone.

**CI guard:** a test/clippy lint (allow-list of not-yet-migrated modules) fails if a raw
`eprintln!`/`println!` appears outside `src/output/` (the `ui::` home) in a migrated module.
The allow-list shrinks with each `MIGRATE-*` FEAT until empty.
