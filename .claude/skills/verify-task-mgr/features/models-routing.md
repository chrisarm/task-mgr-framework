# Models routing

Models routing is the operator-facing config for which provider and capability tier the loop will use. `models init` writes the default block, `models show` / `models list` render it, and `models set-anchor` moves the difficulty window.

## Sub-features

- `models-init` writes the FR-001 `models` + `routing` block into `--dir`/`config.json`.
- `models-show` prints `primaryProvider`, `anchor`, provider ladders, and the Codex route-only note.
- `models-list-offline` prints built-in ladders without calling Anthropic.
- `models-set-anchor` changes the anchor tier and is visible on the next `show`.

## How to get to it (user POV)

- Run `task-mgr init` so a project `config.json` exists (optional but matches operators).
- Run `task-mgr models init`.
- Run `task-mgr models show`.
- Run `task-mgr models list` (no `--remote`).
- Run `task-mgr models set-anchor cheapest` (or `cost-efficient` / `standard` / `frontier`).

## Driving it with verify-task-mgr

Preconditions:

- Fresh sandbox. `$H capture models-project -- --format json init` created `VERIFY_TASK_MGR_DIR/config.json`.
- `$H doctor` isolation ok.
- Do not pass `--remote` or `--refresh`.

- **Write defaults.** Run `$H capture models-init -- models init`. Exit code `0`. Stdout contains `Wrote the FR-001 default models/routing block to .task-mgr/config.json.` `VERIFY_TASK_MGR_DIR/config.json` contains `"models"` and `"anchor"`.
- **Show table.** Run `$H capture models-show -- models show`. Exit code `0`. Stdout contains `primaryProvider: claude`, a line `anchor:` with `standard` (the default), and `Codex pinning is route-only`. Stdout also contains `db_dir:` pointing at `VERIFY_TASK_MGR_DIR` and `source: cli`.
- **Offline list.** Run `$H capture models-list -- models list`. Exit code `0`. Stdout names the built-in providers/ladders and mentions `--remote` as the live catalog opt-in. It does not require `ANTHROPIC_API_KEY`.
- **Move the anchor.** Run `$H capture models-anchor -- models set-anchor cheapest`. Exit code `0`. Stdout contains `Set anchor tier to cheapest`.
- **Show after mutation.** Run `$H capture models-show-2 -- models show`. Exit code `0`. `anchor:` is `cheapest`. `config.json` has `"anchor": "cheapest"`.
- **Proof.** Keep `models-init.*`, `models-show.*`, `models-anchor.*`, `models-show-2.*`. Optionally copy `config.json` into the evidence dir. After cleanup, the captures still exist; the sandbox config is gone (expected).

## Gotchas

- `models` verbs ignore `--format json` and always print text (or write config). Do not `json.loads` their stdout.
- `models enable <provider>` probes that provider's CLI binary *before* writing. A missing binary fails the command and must not be treated as a config bug. `models disable` never probes.
- There is no `models set-primary`. `primaryProvider` is edited in `config.json` or written by `models init`.
- `models list --remote` requires `ANTHROPIC_API_KEY` and `TASK_MGR_USE_API=1`. The helper unsets `TASK_MGR_USE_API`. Do not re-enable it for default proofs.
- `models init --dry-run` prints a diff and must not change `config.json`. If you drive it, compare the file bytes before and after.
- Enabling grok does not disable claude (sparse merge onto builtin defaults). Operators who want grok-only must also `models disable claude` and set `primaryProvider`.
