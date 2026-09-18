# Add and current `--from-json` pin

`add --from-json` and `current --from-json` pin an already-registered effort (not an import). Writes and `target=` go to the PATH you pass. Unregistered, missing, and directory paths are refused. When ≥2 non-NULL prefixes are registered and nothing pins, unpinned `add` refuses; zero-prefix / `--no-prefix` still inserts.

## Sub-features

- `add-pin-happy` pins a registered PRD and appends the new task to that JSON plus the DB.
- `add-unregistered-refuse` refuses an orphan JSON path and inserts no row.
- `add-missing-refuse` refuses a path that does not exist.
- `add-directory-refuse` refuses a directory path.
- `add-multi-prefix-refuse` refuses unpinned `add` when ≥2 non-NULL prefixes are registered.
- `add-no-prefix-insert` still inserts when the DB has zero non-NULL prefixes (`loop init --no-prefix`).
- `current-pin-registered` prints `source=from-json` and `target=` as the pinned PATH.
- `current-pin-unregistered` errors and names `loop init`.
- `help-pin-copy` — `add --help` and `current --help` describe the pin (not import).

## How to get to it (user POV)

- Run `task-mgr loop init tasks/<prd>.json` (with a `taskPrefix`) so the effort is registered.
- Run `echo '{...}' | task-mgr add --stdin --from-json tasks/<prd>.json`.
- Run `task-mgr current --from-json tasks/<prd>.json`.
- Run `task-mgr add --help` / `task-mgr current --help` to read the pin copy.
- For multi-prefix: register two efforts with distinct `taskPrefix` values, then try unpinned `add --stdin`.

## Driving it with verify-task-mgr

Preconditions:

- `$H launch` built this checkout's binary. `$H sandbox-new` active. `$H doctor` isolation ok.
- `PRD=$($H env-print | sed -n 's/^VERIFY_TASK_MGR_PRD=//p')` (absolute sandbox copy of `sample_prd.json`, `taskPrefix` `3019e47c`).
- Helper unsets `TASK_MGR_ACTIVE_PREFIX` — do not re-export it. Pass absolute paths; the helper does not `cd` into the sandbox project.
- Do **not** claim worktree live-path cases here (FEAT-005 rust tests). Do **not** drive `loop run`.

- **Help pin copy.** Run `$H capture add-help -- add --help`. Exit code `0`. Stdout contains `Pin this already-registered effort`. Run `$H capture current-help -- current --help`. Exit code `0`. Stdout contains `Pin this already-registered effort`.
- **Register one prefixed PRD.** Run `$H capture pin-init -- --format json loop init "$PRD"`. Exit code `0`. `"tasks_imported":7`. Do **not** pass `--no-prefix` (need a non-NULL prefix for pin match (a) and later multi-prefix).
- **Happy pin + append.** Run `$H capture add-pin -- --format json add --stdin --from-json "$PRD"` with stdin `{"id":"CODE-FIX-001","title":"verify pin append","difficulty":"medium","touchesFiles":["src/pin_proof.rs"]}`. Exit code `0`. Stderr (or first active line) names `source=from-json` and `target=` containing the PRD path. `$H sql "SELECT id FROM tasks WHERE id LIKE '%CODE-FIX-001'"` returns one row (prefixed `3019e47c-CODE-FIX-001`). `python3 -c "import json; d=json.load(open('$PRD')); print(any(s.get('id','').endswith('CODE-FIX-001') for s in d['userStories']))"` prints `True`. `$H snapshot-db after-pin-add`.
- **Current registered pin.** Run `$H capture current-pin -- --format json current --from-json "$PRD"` (or text). Exit code `0`. Output contains `source=from-json` and `target=` equal to the canonical PRD path.
- **Unregistered refuse.** Write `$VERIFY_TASK_MGR_PROJECT/tasks/orphan.json` with a distinct `taskPrefix` (`ORPHAN99`) and empty `userStories`. Run `$H capture add-unreg -- --format json add --stdin --from-json "$VERIFY_TASK_MGR_PROJECT/tasks/orphan.json"` with stdin `{"id":"ORPHAN-001","title":"should not land"}`. Exit non-zero. Stderr contains `not a registered task_list` and `loop init`. `$H sql "SELECT COUNT(*) FROM tasks WHERE id LIKE '%ORPHAN-001'"` is `0`.
- **Current unregistered.** Run `$H capture current-unreg -- current --from-json "$VERIFY_TASK_MGR_PROJECT/tasks/orphan.json"`. Exit non-zero. Stderr contains `loop init` and `not a registered task_list`.
- **Missing refuse.** Run `$H capture add-missing -- --format json add --stdin --from-json "$VERIFY_TASK_MGR_PROJECT/tasks/no-such.json"` with the same stdin shape. Exit non-zero. Stderr contains `does not exist`.
- **Directory refuse.** Run `$H capture add-dir -- --format json add --stdin --from-json "$VERIFY_TASK_MGR_PROJECT/tasks"` with the same stdin shape. Exit non-zero. Stderr contains `not a regular file` (or `directory`).
- **≥2-prefix unpinned refuse.** Create `$VERIFY_TASK_MGR_PROJECT/tasks/second_prd.json` as a copy of the sample with `"taskPrefix":"aabbcc01"` (and a short `userStories` list, or the full copy). Run `$H capture pin-init-2 -- --format json loop init "$VERIFY_TASK_MGR_PROJECT/tasks/second_prd.json"` — **without** `--no-prefix`. Exit code `0`. Then run `$H capture add-multi-refuse -- --format json add --stdin` with stdin `{"id":"LEAK-001","title":"must refuse","difficulty":"low","touchesFiles":[]}`. Exit non-zero. Stderr names `registered prefixes` and `--from-json`. `$H sql "SELECT COUNT(*) FROM tasks WHERE id LIKE '%LEAK-001'"` is `0`. (Do **not** prove this with two `--no-prefix` imports — that yields zero known prefixes and the refuse never fires.)
- **Zero-prefix insert still works.** `$H cleanup` then `$H sandbox-new --replace`. `$H doctor` isolation ok. Copy the seeded PRD to `$VERIFY_TASK_MGR_PROJECT/tasks/noprefix_prd.json` and **delete** the `taskPrefix` key (sample's `3019e47c` would otherwise land in `prd_metadata` via `file_prefix.or(prd.task_prefix)` even under `--no-prefix`). Run `$H capture noprefix-init -- --format json loop init "$VERIFY_TASK_MGR_PROJECT/tasks/noprefix_prd.json" --no-prefix`. Exit code `0`. `$H sql "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix IS NOT NULL"` is `0`. Run `$H capture add-noprefix -- --format json add --stdin` with stdin `{"id":"BARE-001","title":"zero prefix insert","difficulty":"low","touchesFiles":[]}`. Exit code `0`. `$H sql "SELECT id FROM tasks WHERE id='BARE-001'"` returns `BARE-001` (no auto-prefix). `$H snapshot-db after-noprefix-add`.
- **Proof.** Keep the `add-help`, `current-help`, `pin-init`, `add-pin`, `current-pin`, `add-unreg`, `current-unreg`, `add-missing`, `add-dir`, `pin-init-2`, `add-multi-refuse`, `noprefix-init`, `add-noprefix`, and `after-*` snapshot artifacts (pin cluster and noprefix cluster may be separate run-ids after `--replace`). After `$H cleanup`, those files still exist under `artifacts/<run-id>/`.

## Gotchas

- Helper unsets `TASK_MGR_ACTIVE_PREFIX`. Multi-prefix and pin proofs must use `--from-json` / bare unpinned `add`, not env.
- Seeded `sample_prd.json` has `taskPrefix` `3019e47c`. `loop init` without `--no-prefix` ignores that JSON value and writes a deterministic prefix into the file. Two `--no-prefix` imports of the **same** sample do **not** create ≥2 known prefixes (and may still leave one non-NULL prefix from the JSON fallback). For ≥2-prefix refuse: two inits **without** `--no-prefix` on distinct files (deterministic prefixes differ by filename/branch). For true zero-prefix insert: strip `taskPrefix` before `loop init --no-prefix`.
- `--from-json` on add/current is a pin, not an import. It never registers a PRD. Unregistered copy names `loop init`.
- Absolute sandbox paths only. Relative `tasks/<prd>.json` resolves against the checkout cwd, not the sandbox project.
- Worktree live-path / remap existence matrix is FEAT-005 rust tests — out of scope for this harness.
- Do not invent a second sandbox that isolates `HOME` without keeping `RUSTUP_HOME` / `CARGO_HOME`; use this helper as-is (learning 5443).
