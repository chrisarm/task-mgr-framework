# Prompt: Plan the `task-mgr init` split (project-level vs PRD-level)

> Run this prompt in a fresh Claude Code session **inside the `task-mgr` repo** (not from mw-datalake). It is a **planning** prompt — you are asked to produce a design + migration plan, not code. Do not edit any source files; do not run `cargo` builds. The deliverable is a written plan.

---

## Goal

Refactor `task-mgr`'s top-level `init` command. Today, `task-mgr init --from-json <prd>.json` does two unrelated things:

1. **Project-level setup** — opens-or-creates `.task-mgr/tasks.db`, runs schema migrations, registers config defaults. (Implicit; happens via `open_and_migrate` in `src/db/connection.rs:58` on every command.)
2. **PRD import** — parses a PRD JSON, assigns/writes-back `taskPrefix`, prefixes IDs, inserts tasks/links into the DB.

These should be separated:

- `task-mgr init` — project-level. No PRD required. Idempotent. Creates `.task-mgr/`, runs migrations, writes a default `.task-mgr/config.json` if absent, optionally records the project's root branch / repo metadata. Safe to run repeatedly.
- `task-mgr loop init <prd.json> [flags]` — moves today's `task-mgr init --from-json <prd>.json` behavior here. Per-PRD scope. Inherits today's flag set (`--append`, `--update-existing`, `--force`, `--dry-run`, `--prefix`, `--no-prefix`).
- `task-mgr batch init <glob>... [flags]` — same mechanism, multiple PRDs at once. Matches the existing `task-mgr batch` shape so operators can "init then run" symmetrically (`batch init 'tasks/*.json'` → `batch 'tasks/*.json' --yes`).

The current top-level `init` should keep working **as a deprecated alias** for at least one release: `task-mgr init --from-json X` forwards to `task-mgr loop init X` and prints a deprecation notice to stderr. (See gotcha §2 below — call sites are everywhere.)

---

## Required deliverables (in your response)

1. **Final CLI shape** — `clap` `Subcommand` sketch (Rust-pseudocode is fine) for:
   - The new `Init` (top-level, project-only)
   - `Loop { Init { ... }, Run { ... }, ... }` — i.e. `loop` becomes a parent command with subcommands; the existing `loop` flags move under `loop run`
   - `Batch { Init { ... }, Run { ... }, ... }` — same shape
   - The deprecation shim from top-level `init --from-json` → `loop init`
2. **Migration plan for every existing call site** — see gotcha §2.
3. **Behavioral decisions** the planner must commit to (with rationale), enumerated in gotcha §1-§10 below.
4. **Test strategy** — which existing tests in `src/cli/tests.rs` and `src/commands/init/tests.rs` must be rewritten; what new tests are required; how the deprecation shim is regression-tested.
5. **Risks + a rollback plan** — what failure mode triggers a revert? How is a partial deploy (CLI updated, docs not) recovered from?
6. **Out-of-scope list** — anything you considered but explicitly defer (e.g. moving `taskPrefix` write-back to a different command). Each must include a "why deferred".

Keep the plan ≤ ~600 lines. If you find you need more, the design is probably wrong — flag it.

---

## Current shape (anchors for the plan)

| Concern | File:line |
|---|---|
| Top-level `Init` variant | `src/cli/commands.rs:66-97` |
| `Loop` variant | `src/cli/commands.rs:809-861` |
| `Batch` variant | `src/cli/commands.rs:895-936` |
| `init` dispatch in main | `src/main.rs:116` |
| `init` impl | `src/commands/init/mod.rs:182` (`pub fn init`) |
| PRD parsing | `src/commands/init/parse.rs` |
| Prefix write-back to JSON | `src/commands/init/mod.rs:~110` ("Failed to read {} for prefix write-back") |
| DB open + migrate (already idempotent) | `src/db/connection.rs:58` (`open_and_migrate`) |
| Model picker triggered by `init` | `CLAUDE.md` line 63 (interactive picker fires from `task-mgr init` when nothing resolves) |

---

## Gotchas / lessons learned — the planner MUST address each

### §1. The mid-loop sync footgun is load-bearing

`task-mgr init --from-json tasks/<prd>.json --append --update-existing` is **the canonical way to sync JSON changes into a running effort without destroying status**. This is documented in:

- `~/.claude/CLAUDE.md` § 3a "task-mgr Workflow Patterns" (user-global instructions baked into every Claude Code session)
- `~/.claude/commands/tasks.md` and `~/.claude/commands/plan-tasks.md`
- Every project's `docs/TASK_MGR.md` (mw-datalake has it at lines 36, 208, 211, 214, 235)
- Project-level `CLAUDE.md` files

**Constraint:** after the refactor, the equivalent command (`task-mgr loop init <prd> --append --update-existing`) MUST have byte-for-byte identical semantics on existing rows: preserves `status`, `started_at`, `completed_at`; refreshes `description`, `acceptanceCriteria`, `notes`, `files`, relationships, `humanReviewOutcome`. The bare form (no flags) MUST still be destructive (so the existing "never run bare" rule keeps teaching).

If you propose changing flag defaults (e.g. making `--append` the default), the plan must explicitly call out that this is a behavior change and enumerate the documentation that has to flip in lockstep.

### §2. Call sites are everywhere — produce a migration matrix

Grep results from `mw-datalake` alone:

- `docs/TASK_MGR.md` lines 36, 208, 211, 214, 235
- `~/.claude/CLAUDE.md` lines 71, 75-76, 83, 104
- `~/.claude/commands/tasks.md` lines 275, 489, 961
- `~/.claude/commands/plan-tasks.md` lines 355, 496
- task-mgr's own `README.md` lines 64, 328, 465
- task-mgr's own `CLAUDE.md` lines 63, 75

Plus: `claude-loop.sh` in `mw_support` (large bash script — grep it), every project's `CLAUDE.md`, every slash-command playbook in `~/.claude/commands/`, generated `task-mgr completions <shell>` output, and `cargo run --bin gen-docs` which regenerates `.claude/commands/tasks.md` (CI gates on `gen-docs --check`).

**Deliverable:** the migration matrix must list each file/line, classify it as (a) operator doc → update text, (b) Claude-loaded instruction → update text + add migration note for in-flight loops, (c) shell script → update + version-gate, (d) generated → regenerate via `gen-docs`. Mark which require a coordinated commit across repos.

The deprecation shim is what lets these update on independent cadences. Do NOT plan a flag day.

### §3. The `taskPrefix` auto-write-back is a side effect of `init`

`PrefixMode::Auto` (`src/commands/init/mod.rs:55-64`) **writes back** the auto-generated `taskPrefix` into the PRD JSON file for stability across re-imports. This is documented as a non-obvious behavior in `~/.claude/commands/tasks.md:275` and `~/.claude/commands/plan-tasks.md:355` — they explicitly tell PRD generators not to set `taskPrefix` themselves because `task-mgr init` will assign it.

**Constraint:** this PRD-touching side effect belongs in `loop init`, NOT in project-level `init`. The new top-level `init` must NOT touch any PRD JSON. The plan must spell out where the write-back lives post-refactor.

### §4. Per-worktree DB vs project-level scope mismatch

Each git worktree gets its own `.task-mgr/tasks.db`. So "project-level init" is actually "per-worktree-root init" in practice. Decide and document one of:

- **(a) Keep per-worktree.** `task-mgr init` scaffolds `.task-mgr/` in whichever directory it's run from. Document that running in a worktree creates a worktree-local DB. (Matches today.)
- **(b) Promote to truly project-level.** Single DB at the repo root; worktrees share it via locking or per-branch namespacing. Massive scope expansion — out of scope for this refactor.

Strongly default to (a). If the planner picks (b), they need a separate PRD; flag it and stop.

### §5. `open_and_migrate` already runs implicitly on every command

`src/db/connection.rs:58` opens-or-creates the DB and runs schema migrations as a side effect of *any* command (`next`, `list`, `learn`, etc.). So if a user skips `task-mgr init` and runs `task-mgr next` directly, the DB still appears.

Decide: does the new top-level `init` become **mandatory** (other commands error with "run `task-mgr init` first") or **optional** (lazy-create continues, `init` is just an explicit ergonomic entry point)?

Strongly default to **optional / explicit-but-not-mandatory**. Breaking lazy-create breaks every script and the entire learning capture pipeline. The planner must justify any other choice.

### §6. `--force` blast radius after the split

Today, `task-mgr init --from-json X --force` drops existing data. After the split:

- `task-mgr init --force` (project-level) → what does this drop? **Default: error / refuse.** A project-level reset is `rm -rf .task-mgr/`. Don't add a footgun.
- `task-mgr loop init X --force` → drops THIS PRD's tasks only (today's behavior, scoped). Make the scoping explicit in `--help`.

The plan must state both behaviors and the rationale.

### §7. The interactive model picker is wired to `task-mgr init`

`CLAUDE.md` line 63: "The interactive picker fires from `task-mgr init` when nothing resolves and stdin+stderr are both TTYs." It writes the resolved default to `.task-mgr/config.json` (project-level) or `$XDG_CONFIG_HOME/task-mgr/config.json` (user-level).

**Constraint:** the model picker is project-level config, not PRD-level. It belongs on the NEW `task-mgr init`, not on `loop init`. But operators who run `task-mgr init` today expect the picker to fire on PRD import. The shim must preserve that — `task-mgr init --from-json X` (deprecated) should trigger the picker if no model is resolved, by delegating to BOTH the new `init` and `loop init`.

### §8. Multi-PRD in one shot

Today's `--from-json` is `Vec<PathBuf>` with `required = true` — operators can pass `--from-json a.json --from-json b.json`. Decide for `loop init`: does it take one PRD only (cleaner) or keep the multi-PRD form? If single-PRD, `batch init` is the multi-PRD entry point.

Recommend: `loop init` takes ONE PRD; `batch init <patterns...>` takes a glob/list. Matches the existing `loop` vs `batch` split.

### §9. Tests are dense around `Commands::Init`

`src/cli/tests.rs` lines 2194-2467 are full of `Commands::Loop { ... }` pattern matches and there are sibling tests for `Init`. The init unit tests live in `src/commands/init/tests.rs`. The plan must call out:

- Which existing tests survive verbatim (the import logic itself is unchanged)
- Which need to move (the CLI-parsing tests)
- New tests required: deprecation-shim equivalence, `loop init` ≡ `init --from-json` (golden-file comparison), `init` (project-level) is idempotent, `init` (project-level) does not touch any PRD JSON

### §10. CI gates: `gen-docs --check` and shell completions

`cargo run --bin gen-docs -- --check` fails CI on stale `.claude/commands/tasks.md`. After the refactor, `gen-docs` must regenerate that file AND the shell completions emitted by `task-mgr completions <shell>`. The plan must include a step "run `gen-docs` and commit the regenerated output in the same PR" — otherwise CI breaks on merge.

### §11. The deprecation-shim window

How long does `task-mgr init --from-json X` keep working? Pick one and justify:

- (a) **Indefinitely.** Cheap to maintain; never breaks downstream.
- (b) **Two minor releases**, then remove. Forces ecosystem migration; risks breaking auto-loop runs that haven't updated docs/scripts.
- (c) **One minor release.** Aggressive; reserved for cases with no other choice.

Recommend (a) — the cost is one match arm; the operator-facing pain of (b)/(c) is significant.

### §12. Auto-loop iteration prompts reference `task-mgr init`

`~/.claude/commands/plan-tasks.md:496` includes the literal string `task-mgr init --from-json ... --append --update-existing` inside the prompt that runs inside autonomous loops. If a loop is in-flight when the shim is removed, the loop's runs will start failing on the next iteration. Plan accordingly — see §11.

### §13. The PRD-JSON-in-two-places worktree footgun stays load-bearing

Documented in task-mgr's own `CLAUDE.md` line 75 and mw-datalake `docs/TASK_MGR.md` § 6. After the refactor, `loop init` and `batch init` must still read from whatever working directory they're invoked in — operators rely on `cd <worktree> && task-mgr loop init <prd>` to sync to the worktree's DB. Do not centralize PRD reads through the main repo.

---

## Non-goals (do not expand into these)

- Changing the PRD JSON schema.
- Moving the SQLite DB location.
- Adding a `task-mgr project` parent command (it's natural but doubles the surface area; defer).
- Cross-worktree DB sharing (gotcha §4).
- Removing the auto-prefix write-back (gotcha §3) — keep behavior, just relocate.
- Changing learning capture, recall, or the loop engine — only CLI surface + the `init` command body move.

---

## Format of your response

```
# Plan: Split `task-mgr init` into project-level vs PRD-level

## 1. Final CLI shape
   <Rust pseudocode for Commands enum>

## 2. Migration matrix
   | File | Lines | Class | Action | Coordinated with |

## 3. Behavioral decisions
   §1. Mid-loop sync → <decision + rationale>
   §2. Call sites → <strategy>
   ... (one entry per gotcha)

## 4. Test strategy
   ...

## 5. Risks + rollback
   ...

## 6. Out-of-scope (deferred)
   - <item> — why deferred
```

Keep it tight. The goal is a plan a senior engineer can implement in a single PR (+ a docs PR) without further design discussion.
