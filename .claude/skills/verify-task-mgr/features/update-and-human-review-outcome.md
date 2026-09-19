# Update and `humanReviewOutcome`

`task-mgr update` load-merge-writes whitelist overlay fields (including JSON-only `humanReviewOutcome`) without touching `tasks.status` / `priority` / `archived_at`. Overlay `status`/`passes`/unknown keys/type-null reject fail-closed. `--from-json` pins an already-registered effort. Unpinned update refuses when ≥2 non-NULL prefixes are registered; `--no-prefix` with exactly one `task_list` still succeeds.

## Sub-features

- `update-help-pin` — `update --help` describes the pin (not import).
- `edit-hint-update` — wrong-name `edit` hint names `update --stdin`.
- `notes-only-preserve` patches notes; SQL title/priority/status stay unchanged.
- `passes-blob-reject` rejects a full story blob that includes `passes` (no SQL change).
- `unknown-key-reject` hard-errors unknown overlay keys.
- `type-null-reject` hard-errors empty `title` or `dependsOn: null`.
- `human-review-outcome-persist` writes `humanReviewOutcome` to JSON; survives `loop init --append --update-existing`; no DB column.
- `update-pin-from-json` pins writes via `--from-json` to a registered PATH.
- `update-multi-prefix-refuse` refuses unpinned update when ≥2 non-NULL prefixes are registered.
- `update-no-prefix-succeeds` succeeds when the DB has zero non-NULL prefixes and exactly one `task_list` (`loop init --no-prefix`).
- `json-only-empty-path-refuse` — JSON-only overlay (`id` + `humanReviewOutcome`) with missing/empty write path is `invalid_state` (not Ok-skip). Distinct from the `--no-prefix` succeeds case.

## How to get to it (user POV)

- Run `task-mgr loop init tasks/<prd>.json` (with a `taskPrefix` / Auto sticky prefix) so the effort is registered.
- Run `echo '{"id":"…","notes":"…"}' | task-mgr update --stdin`.
- Run `echo '{"id":"CLARIFY-001","humanReviewOutcome":{…}}' | task-mgr update --stdin --from-json tasks/<prd>.json`.
- Run `task-mgr update --help` to read the pin copy.
- Run a typo `task-mgr edit` to see the `update --stdin` hint.
- For multi-prefix: register two efforts with distinct prefixes, then try unpinned `update --stdin`.
- For CLARIFY docs: checkout `CLAUDE.md` fenced block (not this sandbox) names `update --stdin` then `complete`.

## Driving it with verify-task-mgr

Preconditions:

- `$H launch` built this checkout's binary. `$H sandbox-new` active. `$H doctor` isolation ok.
- `PRD=$($H env-print | sed -n 's/^VERIFY_TASK_MGR_PRD=//p')` (absolute sandbox copy of `sample_prd.json`, seeded `taskPrefix` `3019e47c`).
- Helper unsets `TASK_MGR_ACTIVE_PREFIX` — do not re-export it. Pass absolute paths; the helper does not `cd` into the sandbox project.
- Do **not** claim worktree live-path cases here (FEAT-007 rust tests). Do **not** drive `loop run`.
- Do **not** claim checkout `CLAUDE.md` from sandbox `--dir` artifacts (FEAT-006 regenerate proof is the in-tree file).

- **Help pin copy.** Run `$H capture update-help -- update --help`. Exit code `0`. Stdout contains `Pin this already-registered effort`.
- **Edit hint.** Run `$H capture edit-hint -- edit` (expect non-zero clap failure). Stderr contains `update --stdin`.
- **Register one prefixed PRD.** Run `$H capture update-init -- --format json loop init "$PRD"`. Exit code `0`. `"tasks_imported":7`. Do **not** pass `--no-prefix` (need a non-NULL prefix for pin match and later multi-prefix). Record a live task id via `$H sql "SELECT id, title, priority, status FROM tasks WHERE id LIKE '%TASK-003'"`.
- **Notes-only preserve.** Capture before: `$H sql "SELECT id, title, priority, status FROM tasks WHERE id LIKE '%TASK-003'"`. Run `echo '{"id":"TASK-003","notes":"verify notes-only"}' | $H capture notes-only -- --format json update --stdin`. Exit code `0`. Re-query the same SQL: `title`, `priority`, and `status` are byte-identical to before; notes changed in JSON (`python3`/`jq` on `$PRD` shows `"notes"` containing `verify notes-only` on the TASK-003 story). `$H snapshot-db after-notes-only`.
- **Full blob with `passes` rejected.** Run `echo '{"id":"TASK-003","title":"x","passes":true,"priority":1}' | $H capture passes-reject -- --format json update --stdin`. Exit non-zero. Stderr names `passes` (lifecycle) / `invalid state`. `$H sql "SELECT title, priority, status FROM tasks WHERE id LIKE '%TASK-003'"` matches the notes-only baseline (no SQL change from this reject).
- **Unknown key rejected.** Run `echo '{"id":"TASK-003","synergyWith":["x"]}' | $H capture unknown-reject -- --format json update --stdin`. Exit non-zero. Stderr contains `unknown keys` (and `synergyWith`).
- **Type/null reject.** Run `echo '{"id":"TASK-003","title":""}' | $H capture title-empty-reject -- --format json update --stdin`. Exit non-zero. Stderr mentions `title` / `non-empty`. Run `echo '{"id":"TASK-003","dependsOn":null}' | $H capture depends-null-reject -- --format json update --stdin`. Exit non-zero. Stderr mentions `dependsOn` / `null`.
- **`humanReviewOutcome` overlay + re-import.** Run `echo '{"id":"TASK-003","humanReviewOutcome":{"resolvedAt":"2026-09-19","resolvedBy":"verify","confirmedValues":{},"deltasFromProposed":[],"additionalRequirements":[]}}' | $H capture hro-overlay -- --format json update --stdin`. Exit code `0`. `python3 -c "import json; d=json.load(open('$PRD')); s=next(x for x in d['userStories'] if str(x.get('id','')).endswith('TASK-003')); print('humanReviewOutcome' in s and s['humanReviewOutcome'].get('resolvedBy')=='verify')"` prints `True`. `$H sql "SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='human_review_outcome'"` is `0` (no column). Then run `$H capture hro-reimport -- --format json loop init "$PRD" --append --update-existing`. Exit code `0`. Re-check jq/python: the story **still** has `humanReviewOutcome.resolvedBy == verify` (serde must not strip on re-import).
- **`--from-json` pin.** Run `echo '{"id":"TASK-003","notes":"pinned update"}' | $H capture update-pin -- --format json update --stdin --from-json "$PRD"`. Exit code `0`. Stderr (or first active line) names `source=from-json` and `target=` containing the PRD path. JSON notes contain `pinned update`.
- **≥2-prefix unpinned refuse.** Create `$VERIFY_TASK_MGR_PROJECT/tasks/second_prd.json` as a copy of the sample with `"taskPrefix":"aabbcc01"` (and a short `userStories` list, or the full copy). Run `$H capture update-init-2 -- --format json loop init "$VERIFY_TASK_MGR_PROJECT/tasks/second_prd.json"` — **without** `--no-prefix`. Exit code `0`. Then run `echo '{"id":"TASK-003","notes":"must refuse"}' | $H capture update-multi-refuse -- --format json update --stdin`. Exit non-zero. Stderr names `registered prefixes` and `--from-json`. (Do **not** prove this with two `--no-prefix` imports — that yields zero known prefixes and the refuse never fires.)
- **`--no-prefix` update succeeds.** `$H cleanup` then `$H sandbox-new --replace`. `$H doctor` isolation ok. Copy the seeded PRD to `$VERIFY_TASK_MGR_PROJECT/tasks/noprefix_prd.json` and **delete** the `taskPrefix` key (sample's `3019e47c` would otherwise land in `prd_metadata` via `file_prefix.or(prd.task_prefix)` even under `--no-prefix`). Run `$H capture noprefix-init -- --format json loop init "$VERIFY_TASK_MGR_PROJECT/tasks/noprefix_prd.json" --no-prefix`. Exit code `0`. `$H sql "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix IS NOT NULL"` is `0`. `$H sql "SELECT COUNT(*) FROM prd_files WHERE file_type='task_list'"` is `1`. Run `echo '{"id":"TASK-003","notes":"noprefix ok"}' | $H capture update-noprefix -- --format json update --stdin`. Exit code `0`. `$H sql "SELECT id FROM tasks WHERE id='TASK-003'"` returns `TASK-003`. JSON notes contain `noprefix ok`. `$H snapshot-db after-noprefix-update`.
- **JSON-only empty write path → `invalid_state`.** `$H cleanup` then `$H sandbox-new --replace`. `$H doctor` isolation ok. Re-set `PRD=…`. Run `$H capture jsononly-init -- --format json loop init "$PRD"` **without** `--no-prefix`. Exit code `0`. Move the live task-list file aside so no usable file remains at the registered path: `mv "$PRD" "$PRD.aside"`. Run `echo '{"id":"TASK-003","humanReviewOutcome":{"resolvedBy":"verify"}}' | $H capture jsononly-empty-path -- --format json update --stdin`. Exit non-zero. Stderr contains `invalid state` / `prd json write` (or `no writable task-list path`) and names `task-mgr current` + `--from-json`. This is **not** the `--no-prefix` succeeds case above — do not use `--no-prefix` here.
- **Proof.** Keep the `update-help`, `edit-hint`, `update-init`, `notes-only`, `passes-reject`, `unknown-reject`, `title-empty-reject`, `depends-null-reject`, `hro-overlay`, `hro-reimport`, `update-pin`, `update-init-2`, `update-multi-refuse`, `noprefix-init`, `update-noprefix`, `jsononly-init`, `jsononly-empty-path`, and `after-*` snapshot artifacts (clusters may be separate run-ids after `--replace`). After `$H cleanup`, those files still exist under `artifacts/<run-id>/`. Checkout `CLAUDE.md` (in-tree) still names `update --stdin` under the CLARIFY section — verified outside this harness.

## Gotchas

- Helper unsets `TASK_MGR_ACTIVE_PREFIX`. Multi-prefix and pin proofs must use `--from-json` / bare unpinned `update`, not env.
- Seeded `sample_prd.json` has `taskPrefix` `3019e47c`. `loop init` without `--no-prefix` ignores that JSON value and writes a deterministic prefix into the file. Two `--no-prefix` imports of the **same** sample do **not** create ≥2 known prefixes (and may still leave one non-NULL prefix from the JSON fallback). For ≥2-prefix refuse: two inits **without** `--no-prefix` on distinct files (deterministic prefixes differ by filename/branch). For true zero-prefix insert/update: strip `taskPrefix` before `loop init --no-prefix`.
- `--no-prefix` with exactly one `task_list` is the **succeeds** path for unpinned update. JSON-only `invalid_state` is a **different** case: registered tasks but no usable task-list file (e.g. move the JSON aside after a prefixed init). Do not conflate them.
- `--from-json` on update is a pin, not an import. It never registers a PRD.
- Absolute sandbox paths only. Relative `tasks/<prd>.json` resolves against the checkout cwd, not the sandbox project.
- `humanReviewOutcome` is JSON-only — `pragma_table_info` must show no `human_review_outcome` column. Persistence proof is jq/python on the PRD file, not SQL.
- Worktree live-path / remap existence matrix is FEAT-007 rust tests — out of scope for this harness.
- Checkout `CLAUDE.md` CLARIFY copy is FEAT-006 (`task-mgr enhance agents`); do not treat a sandbox `--dir` CLAUDE.md as that proof.
- Do not invent a second sandbox that isolates `HOME` without keeping `RUSTUP_HOME` / `CARGO_HOME`; use this helper as-is (learning 5443).
