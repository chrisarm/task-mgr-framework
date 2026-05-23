# Engine carve — frozen pre-refactor baselines

Frozen pre-refactor captures of `src/loop_engine/engine.rs` behavior, for the
**Engine Orchestration Boundaries** PRD (`02-engine-orchestration-boundaries`)
dogfood gate (REVIEW-001). REVIEW-001 re-runs `capture.sh` on the post-carve
branch and diffs its output against the `*.txt` files here. The carve is a pure
code-move, so observable behavior must stay identical: the diffs must be empty.

Regenerating requires checking out the pre-refactor commit and re-running the
capture commands below:

- **Captured at:** commit `93fe80f995d8adc54cf5f61c293f4e153b119b9f`
  (branch `refactor/engine-orchestration-boundaries`, **before** any carve task;
  merge-base with `main` is `31b66317`). This branch already carries merged work
  that post-dates `main`, so the honest pre-refactor reference is this HEAD, not
  literal `main`. `engine.rs` is byte-identical to this commit until CONTRACT/
  REFACTOR tasks begin.
- **Build:** `cargo build --bin task-mgr`

## Files

| File                  | What it is                                                            |
| --------------------- | --------------------------------------------------------------------- |
| `capture.sh`          | Deterministic capture harness (the single source of truth)            |
| `mock-runner.sh`      | Stub Claude binary: emits stream-json with a fixed `<completed>` tag   |
| `scenarios/*.json`    | The two input PRDs + their `-prompt.md` files                          |
| `sequential.stderr.txt` / `sequential.db.txt` | Frozen sequential baseline           |
| `wave.stderr.txt` / `wave.db.txt`             | Frozen 2-slot wave baseline          |

## Scenarios

- **sequential** (`--parallel 1`, `--no-worktree`): three tasks in a strict
  dependency chain. Exercises outer `run_loop` + sequential `run_iteration` +
  the per-task completion pipeline. One task per iteration, no wave scheduling.
- **wave** (`--parallel 2`, worktree mode): four independent tasks (disjoint
  `touchesFiles`, no inter-deps) so both slots fill each wave. Exercises
  `run_wave_iteration` + `ensure_slot_worktrees` (slot-0 path threading,
  defense layer #1) + `run_slot_iteration` + `process_slot_result` + merge-back.

These are minimal purpose-built equivalents of the PRD's suggested
`curate-session-cleanup.json` (sequential) and `parallel-task-execution.json`
(wave); the real PRDs are large and timing-noisy, which defeats byte-identical
capture. The minimal scenarios isolate the orchestration seams the carve moves.
Overflow / deadlock / crash defenses are NOT exercised here — they have
dedicated regression tests that move WITH the code per the PRD.

## Regenerate / run the gate

```sh
cargo build --bin task-mgr
B=tests/fixtures/refactor-baselines
$B/capture.sh sequential 1 $B/sequential.stderr.txt $B/sequential.db.txt
$B/capture.sh wave       2 $B/wave.stderr.txt       $B/wave.db.txt
```

At REVIEW-001, capture to scratch files on the post-carve branch and diff:

```sh
$B/capture.sh sequential 1 /tmp/seq.stderr /tmp/seq.db
$B/capture.sh wave       2 /tmp/wave.stderr /tmp/wave.db
diff $B/sequential.stderr.txt /tmp/seq.stderr   # must be empty
diff $B/sequential.db.txt     /tmp/seq.db        # must be empty
diff $B/wave.stderr.txt       /tmp/wave.stderr   # must be empty
diff $B/wave.db.txt           /tmp/wave.db        # must be empty
```

## Determinism contract (what the diffs assert)

`capture.sh` runs each scenario in a **fixed-path** isolated git repo under
`$WORK_BASE` (default `/tmp/carve-baseline`, override with the env var), points
`CLAUDE_BINARY` at `mock-runner.sh`, and disables the usage check + auto-review
(network + a Claude subprocess — both slow and non-deterministic). A
`normalize()` pass then collapses every remaining non-deterministic field to a
stable sentinel: ISO/space timestamps → `<TIMESTAMP>`, run UUIDs → `<UUID>`,
`run_…` ids → `run_ID`, git SHAs → `<SHA>`, durations → `<DURATION>`, the work
path → `<WORK>`.

- **DB-final-state** (`*.db.txt`): `sqlite3 … .dump | sed … | sort`. Sorted, so
  row insertion order is irrelevant; **byte-identical** in both scenarios.
  (Note: `sort` interleaves the fragments of multi-line `CREATE TABLE` /
  `CREATE TRIGGER` statements — harmless, it stays a stable fingerprint.)
- **sequential stderr**: line-ordered and **byte-identical** run to run.
- **wave stderr**: per-slot lines (`[slot 0]` / `[slot 1]`) interleave in
  timing-dependent order, so the contract is **set-equality after
  normalization**. `capture.sh` therefore emits wave stderr **sorted**, so the
  frozen baseline and any re-capture are still directly `diff`-able.

Reproducibility was verified at capture time: each scenario was captured twice
and `diff`'d — all four artifacts empty (see the ANALYSIS-001 progress section).

## Wave-mode stderr ordering (explicit, per AC)

Today's wave stderr is **NOT deterministic line-by-line**. Slots run on
separate threads and flush their banners / `MOCK completing …` / completion
lines as they finish; the interleave flips between waves and between runs
(observed: wave 1 emitted slot-0 before slot-1, wave 2 emitted slot-1 first).
The reproducible contract is **set-equality of normalized lines**, which the
harness pins by sorting. If a future change makes per-slot output serialize in
slot-index order, this can tighten to byte-identical and the `sort` in
`capture.sh` can drop — but that is a behavior change the carve must NOT make.
