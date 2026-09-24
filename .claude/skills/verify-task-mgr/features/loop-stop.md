# Loop stop

`task-mgr loop stop --prefix` asks a live `loop run` / `batch run` to stop by writing the one canonical stop file for that run. It reads a run record under the resolved database directory (`{db_dir}/loop-runs/`), never invents a stop file from the caller's cwd when no live record exists, and does not spawn a loop.

## Sub-features

- `loop-stop-help` shows the nested `loop stop` usage and `--prefix` flag.
- `loop-stop-missing-record` refuses when no live prefix or batch run record exists (non-zero exit, no stop file under the sandbox project).
- `loop-stop-invalid-prefix` refuses empty or slash-containing prefixes before any path join (non-zero exit).
- `loop-stop-live` is skipped: the helper refuses `loop run` / `batch run`, so a live pid + qualifying cmdline cannot be proven here.

## How to get to it (user POV)

- Run `task-mgr loop stop --prefix <prefix>` while a `task-mgr loop run` (or `batch run`) for that prefix is alive.
- Run `task-mgr loop stop --help` to inspect the nested command without touching records.
- A missing or rejected run record exits non-zero and prints candidate directories from any readable record; it does not create `tasks/.stop` under the caller's cwd.

## Driving it with verify-task-mgr

Preconditions:

- `$H launch` so the binary is this checkout's build.
- Fresh `$H sandbox-new` (and `$H doctor` reports isolation ok). Import is optional for help / invalid-prefix; missing-record uses an initialized dir so `--dir` exists.
- Never invoke `loop run` or `batch run` through this harness.

- **Help.** Run `$H capture loop-stop-help -- loop stop --help`. Exit code `0`. Stdout mentions `--prefix` and describes stopping a live run. Does not create files under `$VERIFY_TASK_MGR_DIR/loop-runs/` or any `tasks/.stop*`.
- **Missing record.** After `$H capture loop-stop-init -- --format json loop init "$PRD" --no-prefix` (so the db dir exists), run `$H capture loop-stop-missing -- loop stop --prefix nosuch`. Exit code non-zero. Stderr explains no live loop/batch run was found. Confirm no `tasks/.stop` / `tasks/.stop-nosuch` under `$VERIFY_TASK_MGR_PROJECT` and no `$VERIFY_TASK_MGR_DIR/tasks/.stop`.
- **Invalid prefix.** Run `$H capture loop-stop-bad-prefix -- loop stop --prefix '../x'`. Exit code non-zero. Stderr / error mentions invalid prefix (or empty). Confirm `$VERIFY_TASK_MGR_DIR/loop-runs/` was not created for this refusal (or remains empty of a `../x.json` path join).
- **Live loop (unreachable here).** `$H cli -- loop run "$PRD" --yes` is refused by the helper (exit 2, message `refusing 'loop run'`). Record as `verified-unreachable` with precondition “harness isolation: no agent spawn, no git worktrees on the operator clone.” Do not bypass the helper. Unit tests in `signals::` cover live pid + cmdline stubs.
- **Proof.** Keep `loop-stop-help.*`, `loop-stop-missing.*`, `loop-stop-bad-prefix.*`. After `$H cleanup` they still exist under `artifacts/<run-id>/`.

## Gotchas

- The recipe must never call `loop run` or `batch run`; the helper refuses those and a forced spawn would leave worktrees on the operator clone.
- `cli.dir` is `resolve_db_dir` — default from a worktree anchors at the main repo `.task-mgr`, so records live there, not in a stray worktree-local DB.
- Deprecated flat `task-mgr loop <prd>` has no `run` argv token; `loop stop` fails closed for that process. Do not treat flat form as a live target in this recipe.
- A prefix of `../x` or empty must fail before `Path::join`; do not assert on a joined path that would escape `loop-runs/`.
