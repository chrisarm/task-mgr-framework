---
name: verify-task-mgr
description: "Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change."
---

# Verify task-mgr

task-mgr is a **short-lived CLI** (Rust binary `task-mgr`). There is no server, web UI, or TUI. Each invocation opens SQLite under `--dir`, prints product output, and exits.

Secondary surfaces you must **not** treat as the app: agent skills staged into `$HOME/.claude/commands/` on `init`, and any MCP wrapper around the same CLI. Drive the binary.

This skill is for the next agent, mid-task, who has never seen the repo. Follow it literally. Do not invent a second harness.

Helper path (from the checkout root):

```
H=.claude/skills/verify-task-mgr/scripts/verify-task-mgr
```

## Launch

There is no long-lived process. Launch means: build **this checkout's** binary once, then create an isolated sandbox. Never use `task-mgr` from `PATH` (that is often an older `cargo install`).

```sh
chmod +x .claude/skills/verify-task-mgr/scripts/verify-task-mgr
H=.claude/skills/verify-task-mgr/scripts/verify-task-mgr

$H launch
# Ready when stdout is `task-mgr <semver>` (currently task-mgr 0.3.1) and
# stderr contains `verify-task-mgr: binary <absolute path>`.

$H sandbox-new
# Ready when it prints a run id and `dir ... (not created yet; init will create it)`.
# Sandbox root is `/tmp/task-mgr-verify-<run-id>/` (or `$TMPDIR/task-mgr-verify-<run-id>/`).
```

What the sandbox contains:

| Variable | Path | Role |
|---|---|---|
| `VERIFY_TASK_MGR_PROJECT` | `<root>/project` | Throwaway project root (not this checkout) |
| `VERIFY_TASK_MGR_DIR` | `<root>/project/.task-mgr` | `--dir` (the database directory) |
| `VERIFY_TASK_MGR_HOME` | `<root>/home` | `$HOME` so `init` cannot stage skills into the operator's `~/.claude/commands/` |
| `VERIFY_TASK_MGR_PRD` | `<root>/project/tasks/sample_prd.json` | Copy of `tests/fixtures/sample_prd.json` |
| `VERIFY_TASK_MGR_EVIDENCE` | `.claude/skills/verify-task-mgr/artifacts/<run-id>/` | Proof artifacts (survive cleanup) |

`$H env-print` dumps the live values. Teardown is `$H cleanup` (see Cleanup). Two concurrent drives that share `.current-run` will clobber each other — one sandbox per checkout. Separate `/tmp/task-mgr-verify-*` trees can exist; only the current pointer is active.

## Doctor

Read-only. Run before the first drive, on every fresh sandbox, and whenever output looks wrong.

```sh
$H doctor
```

Worth driving when **all** of these hold:

- `version:` starts with `task-mgr ` and matches `task-mgr --version` from the path `launch` recorded.
- `help: lists Commands`
- After `sandbox-new`: `isolation: ok`, `dir:` is under `/tmp/task-mgr-verify-` (or `$TMPDIR/task-mgr-verify-`), and `home:` is inside that same root — not the operator `HOME`, not `<checkout>/.task-mgr`.
- After any `init` / `loop init`: `db: present` and the printed JSON parses. A freshly imported sample PRD is healthy: `"summary":{"total_issues":0,...}` or text `✓ No issues found. Database is healthy.`

Refuse to drive when doctor fails, when `dir` would be the checkout `.task-mgr`, or when `HOME` would be the operator home. A missing sandbox is OK only for `launch` / binary-only doctor.

## Drive

Every product command goes through the helper so `--dir` and `HOME` cannot drift:

```sh
$H cli -- --format json <subcommand> [args]
$H capture <step-name> -- --format json <subcommand> [args]
```

The helper injects `--dir $VERIFY_TASK_MGR_DIR` and `HOME=$VERIFY_TASK_MGR_HOME`, and unsets `TASK_MGR_DIR`, `TASK_MGR_USE_API`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, and `TASK_MGR_ACTIVE_PREFIX`.

Stable handles (use these, not table row numbers):

| Handle | What it is |
|---|---|
| `--format json` | Global. Machine-readable stdout for `init`, `list`, `show`, `next`, `complete`, `skip`, `fail`, `learn`, `learnings`, `recall`, `run begin`, `doctor`, `status`, `stats`. |
| `models` output | **Text only.** `--format json` is ignored; assert on lines such as `primaryProvider: claude` and `Set anchor tier to cheapest`. |
| Task ids after `--no-prefix` | `TASK-001` … `TASK-007` from the seeded sample PRD. `TASK-001` title is `Create core module structure`. `passes: true` on TASK-001 and TASK-002 imports them as `done`; `next` then returns `TASK-003`. |
| Sample PRD path | `$($H env-print \| sed -n 's/^VERIFY_TASK_MGR_PRD=//p')` or `$H capture` after you copy the value from `env-print`. |
| Product stdout vs stderr | Data (`list` JSON, `models show` text) → stdout. Deprecation notices, skill staging, `Initialized .task-mgr/.` hint → stderr. Capture both. |
| DB side effect | `$H sql 'SELECT id, status, priority FROM tasks ORDER BY priority, id'` and `$H snapshot-db <name>`. |

The helper **refuses** `loop run`, `batch run`, and the deprecated flat `loop <prd>` / `batch <glob>` forms (those *are* run). Autonomous loop spawn is not a default verification path: it creates git worktrees under the operator's clone and launches Claude/Grok/Codex. Prove `loop init` + `status` instead. `loop run --help` is the safe probe.

Read `features/README.md` and drive the feature file for the change under test. A proof that uses one convenient entry point is incomplete when that file lists others.

## Evidence

Put proof in `.claude/skills/verify-task-mgr/artifacts/<run-id>/` (the helper's `VERIFY_TASK_MGR_EVIDENCE`). `capture NAME` writes:

- `NAME.cmd.txt` — exact argv including `--dir`
- `NAME.stdout.txt`
- `NAME.stderr.txt`
- `NAME.exit.txt` — integer
- `snapshot-db NAME` writes `NAME.db.txt`

Proof standards:

- Exercise the real CLI the operator types (`loop init`, `next --claim`, `complete`, `learn`, `models show`). Do not call library functions, do not write SQLite yourself, do not hit test-only binaries.
- Capture the action **and** the resulting state. Example: `complete` stdout plus `show` JSON plus `snapshot-db` (status `done`).
- `--dry-run` is not proof of a no-op by name. After `loop init --dry-run`, observe `SELECT COUNT(*) FROM tasks` is `0`. The DB *file* may still exist because dry-run opens/migrates SQLite; that is expected. The deprecated shim `init --from-json --dry-run` still runs project-level `init_project` (creates `.task-mgr/` + `config.json`) and skips skill staging and task rows — observe those three facts separately.
- `models list --remote` talks to Anthropic. Do not use it. Offline `models list` / `models show` are the operator path.
- `recall --query` needs Ollama and hard-fails without `--allow-degraded`. Prove recall with `--for-task TASK-001` (no network).
- Mocks belong only at production boundaries this repo already isolates (the loop runner). This harness does not spawn runners.

## Cleanup

```sh
$H cleanup
```

Removes only `/tmp/task-mgr-verify-<run-id>/` (the tree `sandbox-new` created) and the `.current-run` pointer. It does **not** kill by process name, does **not** touch the checkout `.task-mgr/`, and does **not** delete `artifacts/<run-id>/`.

After cleanup, confirm `ls .claude/skills/verify-task-mgr/artifacts/<run-id>/` still lists the captured files. If a drive failed, run `cleanup` before retrying `sandbox-new --replace` so a half-written `/tmp` tree is not reused.

There is nothing to SIGTERM on the happy path: every `cli` / `capture` is a short-lived process. If you backgrounded anything outside this helper, that is out of scope — do not `pkill task-mgr`.

## Helpers

`scripts/verify-task-mgr` is executable. From the checkout root:

```sh
H=.claude/skills/verify-task-mgr/scripts/verify-task-mgr
$H launch
$H sandbox-new
$H doctor
$H capture init-loop -- --format json loop init "$PRD" --no-prefix
# PRD is VERIFY_TASK_MGR_PRD from `$H env-print`
$H snapshot-db after-init
$H cleanup
```

| Command | What it does |
|---|---|
| `launch` | `cargo build --bin task-mgr` in this checkout; records the executable path. |
| `sandbox-new [--replace] [--id ID]` | Isolated project + HOME + copied sample PRD. |
| `doctor` | Binary `--version`/`--help`, isolation assertions, optional `doctor` JSON. |
| `cli -- <args>` | Built binary + injected `--dir` + isolated `HOME`. |
| `capture NAME -- <args>` | Same, plus artifacts. Non-zero exit is preserved (needed for expected failures). |
| `sql '<SQL>'` | Query `tasks.db` via Python's sqlite3. |
| `snapshot-db NAME` | Task id/status/priority dump into artifacts. |
| `env-print` | Active sandbox variables. |
| `cleanup` | Delete the sandbox; keep artifacts. |

Seeded sample PRD (`tests/fixtures/sample_prd.json`, copied into the sandbox): 7 stories, `--no-prefix` ids `TASK-001`…`TASK-007`. Import proof: JSON `tasks_imported` = 7, `files_imported` = 14, `relationships_imported` = 6 (dependsOn only; synergy/batch/conflicts are ignored with a stderr warning). TASK-001 and TASK-002 import as `done` because the fixture sets `passes: true`.

Feature map: [features/README.md](features/README.md).
