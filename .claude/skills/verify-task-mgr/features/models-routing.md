# Models routing

Models routing is the operator-facing config for which provider and capability tier the loop will use. `models init` writes the default block, `models show` / `models list` render it, and `models set-anchor` moves the difficulty window. Quota policy verbs (`set-usage-rule`, `set-tier-fallback`, `unset-tier-fallback`) write `usagePolicy.rules` and `routing.tierFallback` without hand-editing JSON.

## Sub-features

- `models-init` writes the FR-001 `models` + `routing` block into `--dir`/`config.json`.
- `models-show` prints `primaryProvider`, `anchor`, provider ladders, and the Codex route-only note.
- `models-list-offline` prints built-in ladders without calling Anthropic.
- `models-set-anchor` changes the anchor tier and is visible on the next `show`.
- `models-set-usage-rule` appends/replaces a `usagePolicy.rules` entry (`--kind` / `--id` + `--on-low`).
- `models-set-tier-fallback` writes `routing.tierFallback` (`maxDifficulty` + include flags).
- `models-unset-tier-fallback` writes JSON `null` (ask opt-out); does **not** delete the key.
- `models-show-policy` (offline) always prints `usagePolicy` + `tierFallback` from config and never prints remaining percents (`% left`).

## How to get to it (user POV)

- Run `task-mgr init` so a project `config.json` exists (optional but matches operators).
- Run `task-mgr models init`.
- Run `task-mgr models show`.
- Run `task-mgr models list` (no `--remote`).
- Run `task-mgr models set-anchor cheapest` (or `cost-efficient` / `standard` / `frontier`).
- Run `task-mgr models set-usage-rule --kind weekly_scoped --on-low unavailable`.
- Run `task-mgr models set-tier-fallback high --include-review --include-forced`.
- Run `task-mgr models show` again (policy lines; no `% left` offline).
- Run `task-mgr models unset-tier-fallback`, then `models show` (`tierFallback: (unset)`).

## Driving it with verify-task-mgr

Preconditions:

- Fresh sandbox. `$H capture models-project -- --format json init` created `VERIFY_TASK_MGR_DIR/config.json`.
- `$H doctor` isolation ok.
- Do not pass `--remote` or `--refresh`.
- Never `loop run` / `batch run`.

- **Help lists new verbs.** Run `$H capture models-help -- models --help`. Exit code `0`. Stdout contains `set-usage-rule`, `set-tier-fallback`, and `unset-tier-fallback`.
- **Write defaults.** Run `$H capture models-init -- models init`. Exit code `0`. Stdout contains `Wrote the FR-001 default models/routing block to .task-mgr/config.json.` `VERIFY_TASK_MGR_DIR/config.json` contains `"models"` and `"anchor"`.
- **Show table.** Run `$H capture models-show -- models show`. Exit code `0`. Stdout contains `primaryProvider: claude`, a line `anchor:` with `standard` (the default), and `Codex pinning is route-only`. Stdout also contains `db_dir:` pointing at `VERIFY_TASK_MGR_DIR` and `source: cli`. Offline show also contains `usagePolicy:` and `tierFallback:` (factory note when omitted) and must **not** contain `% left`.
- **Offline list.** Run `$H capture models-list -- models list`. Exit code `0`. Stdout names the built-in providers/ladders and mentions `--remote` as the live catalog opt-in. It does not require `ANTHROPIC_API_KEY`.
- **Move the anchor.** Run `$H capture models-anchor -- models set-anchor cheapest`. Exit code `0`. Stdout contains `Set anchor tier to cheapest`.
- **Show after mutation.** Run `$H capture models-show-2 -- models show`. Exit code `0`. `anchor:` is `cheapest`. `config.json` has `"anchor": "cheapest"`.
- **Set usage rule.** Run `$H capture models-usage-rule -- models set-usage-rule --kind weekly_scoped --on-low unavailable`. Exit code `0`. Stdout acknowledges the rule. `config.json` contains camelCase `"onLow"` and `"weekly_scoped"`.
- **Set tier fallback.** Run `$H capture models-tier-fallback -- models set-tier-fallback high --include-review --include-forced`. Exit code `0`. `config.json` has `"tierFallback"` with `"maxDifficulty": "high"` and `"includeForced": true`.
- **Show policy (offline).** Run `$H capture models-show-policy -- models show`. Exit code `0`. Stdout contains `usagePolicy:`, `remainingMinPercent:`, `kind=weekly_scoped`, `onLow=unavailable`, and `tierFallback:` with `maxDifficulty=high`. Stdout must **not** contain `% left`.
- **Unset tier fallback.** Run `$H capture models-unset-tier-fallback -- models unset-tier-fallback`. Exit code `0`. `config.json` contains `"tierFallback": null` (key present as null — not deleted).
- **Show after unset.** Run `$H capture models-show-unset -- models show`. Exit code `0`. Stdout contains `tierFallback: (unset)`. Still no `% left`.
- **Proof.** Keep `models-help.*`, `models-init.*`, `models-show.*`, `models-anchor.*`, `models-show-2.*`, `models-usage-rule.*`, `models-tier-fallback.*`, `models-show-policy.*`, `models-unset-tier-fallback.*`, `models-show-unset.*`. Optionally copy `config.json` into the evidence dir. After cleanup, the captures still exist; the sandbox config is gone (expected).

## Gotchas

- `models` verbs ignore `--format json` and always print text (or write config). Do not `json.loads` their stdout.
- `models enable <provider>` probes that provider's CLI binary *before* writing. A missing binary fails the command and must not be treated as a config bug. `models disable` never probes.
- There is no `models set-primary`. `primaryProvider` is edited in `config.json` or written by `models init`.
- `models list --remote` requires `ANTHROPIC_API_KEY` and `TASK_MGR_USE_API=1`. The helper unsets `TASK_MGR_USE_API`. Do not re-enable it for default proofs. Offline `models show` must never print remaining percents; live remaining uses the same opt-in gate as `list --remote`.
- `models init --dry-run` prints a diff and must not change `config.json`. If you drive it, compare the file bytes before and after.
- Enabling grok does not disable claude (sparse merge onto builtin defaults). Operators who want grok-only must also `models disable claude` and set `primaryProvider`.
- `unset-tier-fallback` writes JSON `null` (ask opt-out). Deleting the key would restore factory `Some` — that is the known-bad opposite.
- `set-usage-rule` requires `--kind` and/or `--id` plus `--on-low` (`wait|unavailable|stop|ask|ignore`). Invalid values are CONFIG ERRORs naming the accepted set.
