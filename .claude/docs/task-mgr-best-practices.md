# task-mgr Best Practices

Canonical reference for Claude Code when working with [task-mgr](https://github.com/startat0/task-mgr) loops, batches, PRDs, and task lists.

**Location:** `~/.claude/docs/task-mgr-best-practices.md` (global — available in every project). Staged by `task-mgr init` / `loop init` / `batch init` from the installed binary.

**Skills** (slash commands) are staged at `~/.claude/commands/` by the same init. Refresh after upgrading the binary: `cargo install --path .` then `task-mgr init` (use `--force-skills` if you locally edited a staged file).

---

## Autonomous loop agents

When the prompt contains **"ITERATION SCOPE"** or **"task-mgr"**, you are in loop mode. See also §0 in `~/.claude/CLAUDE.md`.

- Do **not** ask clarifying questions — emit `<promise>BLOCKED</promise>` with a description instead
- Skip waypoints and planning phases — the prompt file is the plan
- Do **not** invoke sub-agents for plan review
- Prefer **`task-mgr` CLI** over reading task JSON (`show`, `list`, `next`, `recall`, `add`, `update`, …)
- Mark iteration outcomes with `<task-status>TASK-ID:done</task-status>` (also: `failed`, `skipped`, `irrelevant`, `blocked`)
- Outside the loop, use `task-mgr complete | fail | skip | unblock | unskip | reset` — there is **no** `task-mgr set-status`

---

## Recommended planning flow (2026)

For most work:

1. Short plan-mode interview (or `/review-plan` if a plan exists)
2. `/spike "…"` when the riskiest assumption is unclear or a design will affect 2+ downstream stories (may emit `CONTRACT-xxx`)
3. `/plan-tasks` (lean) or light `/prd-tasks`
4. Reserve full `/prd` + heavy `/prd-tasks` for large cross-subsystem efforts
5. Run: `task-mgr loop init <prd>.json && task-mgr loop run <prd>.json --yes` (or `batch` for multiple PRDs)
6. After the loop: `/review-loop <prd>.md` → `/compound <prd>.md` on a clean review

PRDs should include **§2.6 Boundary Contracts & Modularity Targets** for `CONTRACT-xxx` tasks (`taskType: "contract"`). Wire dependents with `--depended-on-by CONTRACT-001` (or the PRD milestone).

---

## Essential CLI

| Intent | Command |
|--------|---------|
| Look up a task | `task-mgr show <task-id>` |
| List tasks | `task-mgr list` (`--status`, `--prefix`, `--task-type`) |
| Next eligible task | `task-mgr next` (add `--claim` to mark `in_progress`) |
| Add a task | `echo '<json>' \| task-mgr add --stdin --from-json tasks/<prd>.json` |
| Patch a story | `echo '{"id":"…",…}' \| task-mgr update --stdin --from-json tasks/<prd>.json` |
| Pin context | `task-mgr current --from-json tasks/<prd>.json` |
| Export (active PRD) | `task-mgr export --to-json /tmp/dump.json` (registered dest needs `--force`) |
| Complete / fail / skip | `task-mgr complete \| fail \| skip <id>` |
| Recall learnings | `task-mgr recall --for-task <id>` or `--query "…"` |
| Record a learning | `task-mgr learn --outcome <success\|failure\|workaround\|pattern> --title …` |
| Health check | `task-mgr doctor --auto-fix` |
| Show routing | `task-mgr models show` |

**`--from-json` pins an already-registered effort.** It never registers a new PRD and never remaps the write target. Unregistered path → refuse (`loop init` first). `--depended-on-by` cannot pin a worktree-only file.

**Add task example** (review fixup — always disambiguate the destination PRD):

```sh
echo '{"id":"CODE-FIX-001","title":"Fix race","difficulty":"medium","touchesFiles":["src/foo.rs"]}' \
  | task-mgr add --stdin --from-json tasks/<prd>.json --depended-on-by CONTRACT-001
```

---

## Model routing (config only)

Routing lives in **project** `.task-mgr/config.json`. Never put `model` fields in PRD JSON — they bypass config (per-task) or warn and are ignored (top-level PRD).

**Set routes with the CLI:**

```sh
task-mgr models show                                          # inspect current table
task-mgr models route FEAT --provider codex                   # implementation
task-mgr models route REVIEW --provider grok --tier standard  # reviews → grok-build
task-mgr models route FIX --provider grok --tier cost-efficient  # cheap fixups → composer
task-mgr models unroute <PREFIX>                              # remove a route
```

**Typical operator layout:**

| Prefix | Provider | Tier | Use |
|--------|----------|------|-----|
| `FEAT`, `REFACTOR` | codex | (anchor) | Implementation / refactor gate |
| `REVIEW`, `CODE-REVIEW` | grok | standard | Final & code review |
| `FIX`, `CODE-FIX`, `IMPL-FIX`, `REFACTOR-FIX`, `WIRE-FIX` | grok | cost-efficient | Spawned fixups |

Resolution order: explicit `tasks.model` → `routing.byIdPrefix` → task class → blackout reroute → anchor window → tier→model. See `resolve_execution_plan` in `src/loop_engine/model.rs`.

Codex is **route-only** — never inferred from a model string. Migrate legacy config: `task-mgr models init --force-replace-legacy`.

---

## Mid-loop JSON sync

When the task-list JSON changes mid-effort, **never** run bare `task-mgr init --from-json` — it wipes `status`, `started_at`, and `completed_at`.

**Single-field patches** (notes, description, `humanReviewOutcome`): `task-mgr update --stdin`, not a re-import.

**Canonical bulk sync:**

```sh
task-mgr loop init <prd>.json --append --update-existing --dry-run   # preview
task-mgr loop init <prd>.json --append --update-existing             # apply
```

For multiple PRDs: `task-mgr batch init 'tasks/*.json' --append --update-existing`.

Preserves status on existing rows; refreshes descriptions, criteria, notes, relationships; adds new tasks. Safe on in-progress loops.

**Worktree rule:** operate in the **loop worktree**, not the main repo. Find it with `task-mgr worktrees list`, `cd` there.

---

## Never edit `tasks/*.json` by hand

The loop engine re-imports PRD JSON each iteration. Hand edits corrupt state. Use:

- `task-mgr add --stdin --from-json tasks/<prd>.json` for new tasks
- `task-mgr update --stdin --from-json tasks/<prd>.json` for whitelist overlays (`id` required; `status` / `passes` hard-error)
- `task-mgr loop init … --append --update-existing` for bulk sync
- `<task-status>` tags for status from loop iterations

JSON-sync failure copy names `task-mgr current` and retry `--from-json`, **never** `export`.

---

## Spawn-fixup PRD targeting

When review/refactor gates spawn `CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, or `REFACTOR-FIX-*` tasks, disambiguate the destination PRD:

- `--from-json tasks/<correct-prd>.json` — explicit path (pin)
- `--depended-on-by CONTRACT-001` (or the PRD milestone) — preferred when a contract exists

Wrong targeting leaks orphan `passes: false` placeholders into unrelated PRDs.

---

## Human-in-the-loop (CLARIFY) tasks

When `requires_human: true`, emit `<promise>BLOCKED</promise>` until resolved. On resolution, **do not hand-edit JSON**:

```sh
echo '{"id":"CLARIFY-001","humanReviewOutcome":{
  "resolvedAt": "YYYY-MM-DD",
  "resolvedBy": "<name>",
  "confirmedValues": { },
  "deltasFromProposed": [ ],
  "additionalRequirements": [ ]
}}' | task-mgr update --stdin --from-json tasks/<prd>.json
task-mgr complete CLARIFY-001
```

Downstream field updates in the same resolution also go through `task-mgr update --stdin`. Overlay `status` / `passes` is a hard error (lifecycle SSoT). Then:

1. `task-mgr learn --outcome success --task-id <id> --confidence high`
2. Update the PRD markdown: check off the open question with resolution date and confirmed value

---

## Export (scoped dump)

Default `task-mgr export --to-json PATH` writes the **active PRD only**. `--all` restores dump-all.

Writing onto a **registered** `task_list` always requires `--force`. Export is a **lossy dump** (no `taskPrefix`, extra keys stripped, status collapsed to `passes`) — `--force` is not a merge. Prefer an unregistered dump path, or pin scope with `--from-json`.

Do **not** `export --to-json` onto the live PRD as crash recovery. Loop persistence is `prd_reconcile` / `add` / `update`.

---

## Architectural decisions (`tm-decisions`)

`task-mgr decisions resolve <id> <letter>` records the choice only — it does not edit code.

1. Read the code region first; ratification may be a no-op if code already matches
2. If code reflects the rejected option, implement the chosen approach
3. Empty commits (`git commit --allow-empty`) are fine for ratification-only decisions

---

## Learnings & recall

- `task-mgr learn` — record patterns, failures, workarounds (feeds future recall)
- `task-mgr recall --for-task <id>` — scored for this task (tags, files, errors)
- `task-mgr recall --query "…"` — semantic search (needs Ollama; `--allow-degraded` offline)
- `task-mgr apply-learning` / `invalidate-learning` — UCB bandit feedback

Run recall **before** planning or implementing in unfamiliar areas.

---

## Post-loop review

1. `/review-loop <prd>.md` — backward-looking: was it built correctly?
2. `/compound <prd>.md` — forward-looking: capture learnings, CLAUDE.md gotchas, decisions

---

## Common gotchas

- **Parallel slots:** `--parallel N` (1–3). Conflict = `touchesFiles` overlap. `--parallel 1` is byte-identical to sequential.
- **Stale test binaries after parallel slots:** if `cargo test` fails with paths to a removed `-slot-N` worktree, `touch tests/<binary>.rs` and rebuild — not a code regression (learning #4753).
- **Deprecated shim still works:** `task-mgr init --from-json <prd>` → prefer `task-mgr init && task-mgr loop init <prd>`.
- **`--from-json` is a pin, not import.** Unregistered path refuses. ≥2 prefixes without a pin refuse writes (add/update/export). Zero-prefix / `--no-prefix` still allow DB insert.
- **`task-mgr update` vs `task-mgr run update`:** overlay clap is `task-mgr update --stdin`. Run-session remains `task-mgr run update`.
- **Export smash:** never dump onto a live registered `tasks/<prd>.json` without `--force`, and even then it is lossy — prefer an unregistered dest.
- **Skills refresh:** `task-mgr init --force-skills` overwrites locally modified `~/.claude/commands/` and `~/.claude/docs/task-mgr-best-practices.md` copies.
