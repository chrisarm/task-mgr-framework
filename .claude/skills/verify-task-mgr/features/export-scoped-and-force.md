# Export scoped dump and `--force`

`export` dumps the active PRD by default (not the whole DB). `--from-json` pins an already-registered effort as the dump **source** (dest stays `--to-json`). `--all` restores today's unscoped dump. Overwriting a registered `task_list` refuses without `--force` (lossy dump: no `taskPrefix`, extra keys stripped — not a merge). With ≥2 non-NULL prefixes and no pin, default export errors naming `--from-json`, `--all`, and `task-mgr current`.

## Sub-features

- `export-help-pin` — `export --help` describes the pin (not import) and names `--force` / `--all`.
- `single-prefix-new-dest` dumps the one active PRD to a new (unregistered) file without `--force`.
- `registered-dest-refuse` — `--to-json` onto the registered PRD path without `--force` refuses; dest bytes unchanged.
- `registered-dest-force` — `--force` replaces the registered dest (lossy: no `taskPrefix`).
- `all-two-prefix-dump` — `--all` on a two-prefix DB dumps tasks from both prefixes.
- `multi-prefix-default-refuse` — default export with ≥2 prefixes and no pin errors naming `--from-json`, `--all`, and `task-mgr current`.
- `all-from-json-clap-fail` — `--all --from-json` fails at clap parse.
- `from-json-pin-scoped` — `--from-json` pin dumps only that prefix's tasks.
- `zero-prefix-default-refuse` — zero-prefix DB default export errors (same three names); `--all` to a new file works.

## How to get to it (user POV)

- Run `task-mgr loop init tasks/<prd>.json` (with a `taskPrefix` / Auto sticky prefix) so the effort is registered.
- Run `task-mgr export --to-json /tmp/active-dump.json` for the active-PRD scoped dump to a **new** file.
- Run `task-mgr export --from-json tasks/<prd>.json --to-json /tmp/my-dump.json` to pin the dump source.
- Run `task-mgr export --all --to-json /tmp/full-dump.json` for today's unscoped dump.
- Run `task-mgr export --from-json tasks/<prd>.json --to-json tasks/<prd>.json --force` only when intentionally overwriting a registered task-list (lossy).
- Run `task-mgr export --help` to read the pin / `--force` / `--all` copy.
- For multi-prefix: register two efforts with distinct prefixes (two `loop init`s **without** `--no-prefix`), then try default `export --to-json …`.

## Driving it with verify-task-mgr

Preconditions:

- `$H launch` built this checkout's binary. `$H sandbox-new` active. `$H doctor` isolation ok.
- `PRD=$($H env-print | sed -n 's/^VERIFY_TASK_MGR_PRD=//p')` (absolute sandbox copy of `sample_prd.json`, seeded `taskPrefix` `3019e47c`).
- Helper unsets `TASK_MGR_ACTIVE_PREFIX` — do not re-export it. Pass absolute paths; the helper does not `cd` into the sandbox project.
- Prefer **unregistered** dump paths for happy dumps (e.g. `$VERIFY_TASK_MGR_PROJECT/tasks/export-dump.json`). Dest-new-file vs dest-registered are different proofs.
- Do **not** claim worktree dest-identity cases here (FEAT-010 rust tests). Do **not** drive `loop run` / `batch run`. Do **not** claim `task_ops` prompt copy here (unit test, not this harness).

- **Help pin copy.** Run `$H capture export-help -- export --help`. Exit code `0`. Stdout contains `Pin this already-registered effort` (and names `--force` / dump-not-merge / `--all`).
- **Register one prefixed PRD.** Run `$H capture export-init -- --format json loop init "$PRD"`. Exit code `0`. `"tasks_imported":7`. Do **not** pass `--no-prefix` (need a non-NULL prefix for active default and later multi-prefix).
- **Single-prefix dump to a new file.** Set `DUMP=$VERIFY_TASK_MGR_PROJECT/tasks/export-dump.json`. Assert `DUMP` does not exist (or is not a registered `task_list`). Run `$H capture export-new-dest -- --format json export --to-json "$DUMP"`. Exit code `0`. File exists; `python3 -c "import json; d=json.load(open('$DUMP')); print(len(d['userStories']), 'taskPrefix' in d)"` prints `7 False` (seven stories, no `taskPrefix`). `$H snapshot-db after-single-export`.
- **Registered dest without `--force` refuses.** Capture before bytes: `cp "$PRD" "$PRD.before"`. Run `$H capture export-reg-refuse -- --format json export --from-json "$PRD" --to-json "$PRD"`. Exit non-zero. Stderr contains `--force` and `dump` (dump-not-merge). `cmp -s "$PRD" "$PRD.before"` succeeds (dest unchanged).
- **Registered dest with `--force` replaces (lossy).** Optionally seed an extra key into `$PRD` (e.g. `"extraKeepMe":true`) so loss is observable. Run `$H capture export-reg-force -- --format json export --from-json "$PRD" --to-json "$PRD" --force`. Exit code `0`. `python3 -c "import json; d=json.load(open('$PRD')); print('taskPrefix' in d, 'extraKeepMe' in d)"` prints `False False`. `$H snapshot-db after-force-export`.
- **≥2-prefix default refuse.** Create `$VERIFY_TASK_MGR_PROJECT/tasks/second_prd.json` as a copy of the sample with a distinct filename (optional short `userStories`). Run `$H capture export-init-2 -- --format json loop init --append "$VERIFY_TASK_MGR_PROJECT/tasks/second_prd.json"` — **without** `--no-prefix` (two Auto-prefixed inits; `--append` keeps the first PRD). Exit code `0`. `$H sql "SELECT COUNT(DISTINCT task_prefix) FROM prd_metadata WHERE task_prefix IS NOT NULL"` is `≥2`. Set `OUT=$VERIFY_TASK_MGR_PROJECT/tasks/multi-out.json` (must not exist). Run `$H capture export-multi-refuse -- --format json export --to-json "$OUT"`. Exit non-zero. Stderr contains `--from-json` and `--all` and `task-mgr current`. Assert `OUT` was **not** created. (Do **not** prove this with two `--no-prefix` imports — that yields zero known prefixes and the refuse never fires.)
- **`--all` dumps both prefixes.** Run `$H capture export-all-two -- --format json export --all --to-json "$OUT"`. Exit code `0`. `python3` / `jq` on `$OUT`: `userStories` includes ids from **both** prefixes (first sticky Auto prefix and the second file's sticky prefix). Metadata stamp is first `prd_metadata` row (`ORDER BY id LIMIT 1`) — do not assert both projects' metadata.
- **`--from-json` pin dumps only that prefix.** Set `SCOPED=$VERIFY_TASK_MGR_PROJECT/tasks/scoped-dump.json`. Run `$H capture export-pin-scoped -- --format json export --from-json "$PRD" --to-json "$SCOPED"`. Exit code `0`. Every `userStories[].id` matches the first PRD's sticky prefix (none from the second).
- **`--all --from-json` clap-fails.** Run `$H capture export-all-from-json-clash -- export --all --from-json "$PRD" --to-json "$VERIFY_TASK_MGR_PROJECT/tasks/clash.json"`. Exit non-zero (clap parse). Stderr mentions conflict / cannot be used with (`--all` vs `--from-json`).
- **Zero-prefix default refuse; `--all` works.** `$H cleanup` then `$H sandbox-new --replace`. `$H doctor` isolation ok. Re-set `PRD=…`. Copy the seeded PRD to `$VERIFY_TASK_MGR_PROJECT/tasks/noprefix_prd.json` and **delete** the `taskPrefix` key (sample's `3019e47c` would otherwise land in `prd_metadata` via `file_prefix.or(prd.task_prefix)` even under `--no-prefix`). Run `$H capture noprefix-init -- --format json loop init "$VERIFY_TASK_MGR_PROJECT/tasks/noprefix_prd.json" --no-prefix`. Exit code `0`. `$H sql "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix IS NOT NULL"` is `0`. Set `ZOUT=$VERIFY_TASK_MGR_PROJECT/tasks/zero-out.json` (must not exist). Run `$H capture export-zero-refuse -- --format json export --to-json "$ZOUT"`. Exit non-zero. Stderr contains `--from-json` and `--all` and `task-mgr current`. Assert `ZOUT` was not created. Then run `$H capture export-zero-all -- --format json export --all --to-json "$ZOUT"`. Exit code `0`. File exists with `userStories` length `7`. `$H snapshot-db after-zero-all`.
- **Proof.** Keep the `export-help`, `export-init`, `export-new-dest`, `export-reg-refuse`, `export-reg-force`, `export-init-2`, `export-multi-refuse`, `export-all-two`, `export-pin-scoped`, `export-all-from-json-clash`, `noprefix-init`, `export-zero-refuse`, `export-zero-all`, and `after-*` snapshot artifacts (prefixed cluster and noprefix cluster may be separate run-ids after `--replace`). After `$H cleanup`, those files still exist under `artifacts/<run-id>/`.

## Gotchas

- Helper unsets `TASK_MGR_ACTIVE_PREFIX`. Multi-prefix refuse and pin proofs must use `--from-json` / bare default export / `--all`, not env.
- Do **not** prove ≥2-prefix refuse with two `--no-prefix` imports — that yields zero known prefixes and the refuse never fires. Use two `loop init`s **without** `--no-prefix` on distinct files (deterministic Auto prefixes differ by filename/branch).
- Dest-new-file vs dest-registered: happy dumps target an **unregistered** path (no `--force`). Registered overwrite is a separate refuse / `--force` proof. Do not smash `$PRD` in the new-dest bullet.
- Worktree dest-identity (canonicalize + remap `Some` still requires `--force`) is FEAT-010 rust tests — out of scope for this harness.
- Do not drive `loop run` / `batch run` through this harness.
- `task_ops` jq / add / update one-liners are a unit-test surface, not this harness.
- Seeded `sample_prd.json` has `taskPrefix` `3019e47c`. `loop init` without `--no-prefix` ignores that JSON value and writes a deterministic prefix into the file. For true zero-prefix: strip `taskPrefix` before `loop init --no-prefix`.
- Absolute sandbox paths only. Relative `tasks/<prd>.json` resolves against the checkout cwd, not the sandbox project.
- Do not invent a second sandbox that isolates `HOME` without keeping `RUSTUP_HOME` / `CARGO_HOME`; use this helper as-is (learning 5443).
