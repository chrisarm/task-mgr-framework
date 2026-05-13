---
name: compound
description: "Capture forward-looking learnings from a completed PRD so future PRDs inherit the wisdom. Runs AFTER /review-loop (which is backward-looking). Updates CLAUDE.md gotchas, task-mgr learnings, tm-decisions, copy-voice principles, and writes a daily changelog entry. Flags agent graduations when a pattern has accumulated enough evidence. Triggers on /compound slash command. Takes a PRD markdown filename as argument."
user_invocable: true
---

# /compound — Capture Forward-Looking Learnings

**Runs after `/review-loop`.** While `/review-loop` looks backward ("was this built correctly?"), `/compound` looks forward ("what should future PRDs inherit from this one?").

The first three phases — plan, work, review — produce a feature. The compound phase produces improved infrastructure for the **next** feature. Core principle: every unit of work should make subsequent work easier.

## Usage

```
/compound tasks/prd-<feature>.md
/compound tasks/prd-<feature>.md "also propose an agent for X"
```

## Prerequisites

- The loop has finished (`MILESTONE-FINAL` passed)
- `/review-loop` has completed (code review + coherence check are in the progress log)

If either is missing, tell the user and stop — `/compound` depends on their outputs.

## Context Economy (same rules as `/tasks`)

- **Never Read `tasks/*.json`** — use `jq` for specific fields.
- **The progress log is the ONE exception to the tail rule** — compound needs the full arc of the PRD to extract patterns, so read the file in its entirety. It's bounded to this PRD.
- **Never Read CLAUDE.md cover-to-cover** — `grep -n -A 20` for the subsystem headers this PRD touched.
- **Never Read `tasks/long-term-learnings.md` or `tasks/learnings.md`** — use `task-mgr recall`.

## Where commits land — READ THIS FIRST

**Every artifact this skill writes (CLAUDE.md edits, copy-voice entries, changelog files) MUST be edited inside the PRD's worktree, and the resulting commit MUST land on the worktree branch — NOT on the cwd's branch (which is usually `main`).**

`main` is typically push-protected via repo rules ("changes must come through a pull request"). A `compound:` commit landing on local `main` becomes homeless: it can't be pushed, and the work has to be cherry-picked onto the PRD branch later (with all the disk/conflict friction that implies). The captures belong on the worktree branch so they ship with the PRD's PR alongside the feature work they're commenting on.

**Mechanics:**
- Resolve `WORKTREE=<worktree-path>` once (Step 0 below) and reuse it throughout.
- Edit files at `$WORKTREE/<path>` — each worktree has its own working tree of files. Editing the main repo's `CLAUDE.md` instead of `$WORKTREE/CLAUDE.md` means the change doesn't make it onto the PR branch.
- Use `git -C $WORKTREE add ...` and `git -C $WORKTREE commit ...` for the commit. Don't rely on cwd.
- `task-mgr learn` and `task-mgr recall` are repo-agnostic (they hit a global SQLite DB) — those work the same regardless of cwd.
- `tasks/progress-$PREFIX.txt` lives at the main-repo's tasks/ path (loop-engine state, often gitignored) — append there for human visibility. Don't try to commit it.

## Step 0: Resolve worktree + identifiers

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/<feature>.json)
BRANCH=$(jq -r '.branchName' tasks/<feature>.json)
PRD_TITLE=$(jq -r '.description // .project' tasks/<feature>.json)
TODAY=$(date -I)  # YYYY-MM-DD

# Resolve WORKTREE — parent-directory-sibling -worktrees/, with / replaced by -
# Fallback: git worktree list | grep "$BRANCH" | awk '{print $1}'
WORKTREE=$(git worktree list --porcelain | awk -v b="$BRANCH" '
  /^worktree / { wt=$2 }
  $0 == "branch refs/heads/" b { print wt; exit }
')

# Sanity: every git op below uses git -C $WORKTREE
git -C $WORKTREE branch --show-current  # must equal $BRANCH
```

If `WORKTREE` is empty, ask the user for the path and don't proceed without it.

## Step 1: Gather the reflective inputs

Four narrow pulls:

```bash
# 1. Full progress log for this PRD (exception to the tail rule — compound needs the arc)
cat tasks/progress-$PREFIX.txt

# 2. Commit history for this PRD's branch
git -C $WORKTREE log main..HEAD --oneline

# 3. Learnings already recorded mid-loop for this PRD
task-mgr recall --prefix $PREFIX --limit 30

# 4. Changed files (to scope CLAUDE.md §5 greps and to understand subsystem touch)
git -C $WORKTREE diff --stat main..HEAD
```

## Step 2: Reflective extraction across six categories

For each category ask: **"What should a future PRD inherit from what this PRD learned?"** Capture only what clears that bar. Silence is acceptable for any category that didn't produce durable wisdom.

### 2.1 Gotchas → `CLAUDE.md §5`

Hard-won invariants, footguns, non-obvious constraints — one bullet per rule, subsystem-organized.

Before writing, grep for existing content about each touched subsystem to avoid duplication. **Use the worktree's copy of CLAUDE.md, not the main repo's:**

```bash
grep -n -A 20 '### <Subsystem Name>' $WORKTREE/CLAUDE.md
```

Append new entries under the matching `### <Subsystem>` heading in `$WORKTREE/CLAUDE.md §5`. Format:

```markdown
- **<invariant name>** — <specific rule>. <reason/consequence>. <concrete example if useful>
```

Use `Edit` against the path `$WORKTREE/CLAUDE.md` (NOT the main-repo CLAUDE.md). Never regenerate.

### 2.2 Indexed learnings → `task-mgr learn`

For each distilled learning that's recurrent (expect to see this on future PRDs), record it. Check for duplicates first:

```bash
task-mgr recall --query "<title terms>" --limit 5
```

If a similar learning exists and the new evidence raises confidence, update (or record the same title with `--confidence high` — task-mgr deduplicates by title). Otherwise:

```bash
task-mgr learn \
  --outcome pattern \
  --title "<terse title>" \
  --content "<the wisdom, specific enough to be actionable, general enough to apply across future PRDs>" \
  --tags "<subsystem>,<pattern-kind>" \
  --confidence high|medium|low \
  --files "<optional file paths>"
```

Tag hygiene: reuse existing tags where they fit (`task-mgr recall --list-tags` if you're unsure). Inventing a new tag for every learning defeats the recall index.

### 2.3 Architectural decisions → `tm-decisions`

Only when a choice was **made** during this PRD that future work must honor. Not every PRD produces a decision.

Invoke `/tm-decisions` (or the underlying CLI) to record the decision with its options, the chosen path, and rationale. Cross-reference the PRD filename in the context.

### 2.4 Copy voice → `docs/architecture/copy-voice.md`

If this PRD added or changed bot-reply copy (error messages, confirmations, workflow responses, style-guide entries), capture the voice principle. **Edit `$WORKTREE/docs/architecture/copy-voice.md`**, not the main repo's copy.

If the file doesn't exist on this branch, create it with the seed content at the bottom of this skill (at `$WORKTREE/docs/architecture/copy-voice.md`).

Append a dated entry under `## Captured Rules`:

```markdown
## <YYYY-MM-DD> — <pattern name>

**From PRD**: `<prd-filename>`

**Principle**: <the rule, 1-2 sentences>

**Rationale**: <why — user research, UX feedback, product decision, etc.>

**Example**:
- Before: "<old copy>" (or "N/A — new")
- After: "<new copy>"

**Proposed propagation**: `configs/base.yml style_guides.<path>` → `"<concise rule>"` (**requires human review before deploy**).
```

`/compound` does NOT touch `configs/base.yml` or `configs/workflows/*.yml` in iter 1 — those ship to prod and need human eyes on every diff. The `copy-voice.md` entry is the capture; human PR is the propagation.

### 2.5 Changelog → `docs/change_logs/CHANGELOG_<YYYY-MM-DD>.md`

One file per day, **inside the worktree** so it ships with the PR.

```bash
CHANGELOG=$WORKTREE/docs/change_logs/CHANGELOG_$TODAY.md
mkdir -p $WORKTREE/docs/change_logs
test -f $CHANGELOG || printf '# Changelog — %s\n\n' "$TODAY" > $CHANGELOG
```

If the file already exists in the worktree (e.g., a sibling PRD merged earlier today and your worktree branched off after), append to it. If your worktree branched off before today, the file likely doesn't exist yet on this branch — create it.

Append:

```markdown
## <PRD title>

**Branch**: `<branchName>`
**PRD**: `tasks/prd-<feature>.md`

### What shipped

<1-3 sentences, user-facing if applicable>

### Why it matters

<concrete user benefit>

### Breaking changes

<list, or "None">

---
```

Target audience: internal team + ops. Cap each PRD entry at ~200 words.

### 2.6 Pattern graduation check → potentially a new agent

For each learning recorded in 2.2:

```bash
task-mgr recall --query "<learning title>" --limit 20
```

Count the results where confidence ≥ 50%. If **count ≥ 12**, the pattern has graduated.

**Graduation steps:**

1. **Draft a new agent** at `~/.claude/agents/<proposed-name>.md.draft`:
   - Name based on the pattern's subject (e.g., `adp-timecard-specialist.md.draft`, `workflow-condition-expert.md.draft`).
   - Frontmatter with `name`, `description`, `tools`, `model`.
   - System prompt that encodes the accumulated expertise — distill the ≥12 source learnings into a coherent persona and ruleset.
   - Include in the prompt body: `This agent was graduated from <N> accumulated learnings on <date>. See task-mgr learnings tagged superseded-by:<pointer-id>.`

2. **Record a pointer learning** that supersedes the cluster:
   ```bash
   POINTER_ID=$(task-mgr learn \
     --outcome pattern \
     --title "Use agent <name> for <topic>" \
     --content "<N> accumulated learnings indicate this is a well-understood area; invoke the specialist agent for future work in this space." \
     --tags "<original-tags>,graduated" \
     --confidence high)
   ```
   (Capture the returned ID however `task-mgr learn` surfaces it — may be in stdout or require a follow-up `recall` to find.)

3. **Mark source learnings superseded.** If `task-mgr` supports tag-add on existing learnings, use it:
   ```bash
   for id in $MATCHED_IDS; do
     task-mgr learn-tag $id --add "superseded-by:$POINTER_ID"
   done
   ```
   If it doesn't, **record in the compound report** as a human-follow-up item listing the IDs — do not silently drop this step. This is a known iter-1 gap; iter 2 may add the CLI support.

4. **Flag in the compound report:**
   ```
   AGENT GRADUATION READY
   - Draft: ~/.claude/agents/<name>.md.draft
   - Review, edit if needed, rename to .md to activate.
   - Superseded learnings: <list of IDs>
   - Pointer learning: <pointer-id>
   ```

## Step 3: Verify each artifact

For every artifact written, confirm it's retrievable by the system that'll consume it. Verifications target the **worktree** for file artifacts:

| Artifact           | Verify command                                                                       |
| ------------------ | ------------------------------------------------------------------------------------ |
| `CLAUDE.md §5`     | `grep -n '<keyword from new entry>' $WORKTREE/CLAUDE.md`                             |
| `task-mgr learn`   | `task-mgr recall --tags <new-tag> --limit 3`                                         |
| `tm-decisions`     | `task-mgr decisions list --tag <tag>` (or `/tm-decisions` equivalent)                |
| Copy voice         | `grep -n '<pattern name>' $WORKTREE/docs/architecture/copy-voice.md`                 |
| Changelog          | `grep -n '<PRD title>' $WORKTREE/docs/change_logs/CHANGELOG_$TODAY.md`               |
| Agent draft        | `ls ~/.claude/agents/<name>.md.draft`                                                |

Any verify that fails goes into the compound report as a warning — do not silently continue.

## Step 4: Compound report

Append this block to `tasks/progress-$PREFIX.txt` (the loop-engine progress log lives at the main repo's tasks/ path — it's typically gitignored, so don't try to commit it; just append for human visibility):

```
## <YYYY-MM-DD HH:MM> - COMPOUND
PRD: <prd-filename>
Worktree: $WORKTREE
Branch: $BRANCH
CLAUDE.md: <N entries added> (subsystems: <list>)
Learnings: <N recorded> (tags: <list>)
Decisions: <N recorded> (or "none")
Copy voice: <N principles captured> (or "no voice changes")
Changelog: docs/change_logs/CHANGELOG_<today>.md
Graduations: <none | N drafts ready for review>
Pattern counts approaching threshold (≥8, <12): <list with current counts>

Verify failures: <none | list>
Follow-ups for humans: <list — e.g. "propagate copy-voice entries to configs/base.yml">
---
```

## Step 5: Commit and present

Commit the captured artifacts **inside the worktree** (NOT the agent drafts — those are global and require human approval; and NOT the progress log — that's gitignored):

```bash
git -C $WORKTREE add CLAUDE.md docs/architecture/copy-voice.md docs/change_logs/
git -C $WORKTREE commit -m "compound: forward-looking capture for <prd-name>"
```

If `git -C $WORKTREE add` complains that some path doesn't exist, you edited the main repo's copy by mistake — re-edit the worktree's copy and retry. If `git add` fails with "paths are ignored" on the progress log, that's expected — drop it from the add list.

After committing, the captured commit lives on `$BRANCH` and will ship with the PRD's PR. Do NOT attempt to push from this skill — pushing is a separate user decision (and `main` is push-protected anyway).

Print to stdout a concise summary:

- What compounded across each of the six categories
- Patterns approaching graduation (count vs threshold)
- Agent graduations ready for human review (with the draft file path)
- Any verify failures

End with: `Review above, approve the commit, and this PRD is fully compound-captured.`

---

## Seed content for `docs/architecture/copy-voice.md`

If the file doesn't exist at the start of `/compound`, create it with:

```markdown
# Copy Voice — Bot Reply Style

This document is the source of truth for principles behind how the bot replies to users. Concrete phrasing lives in `configs/base.yml` (`style_guides`) and per-workflow YAMLs under `configs/workflows/`.

## Propagation disclaimer

**Appending to this file does NOT change bot behavior.** Behavior change requires updating:

1. `configs/base.yml style_guides` — category-level voice rules consumed by `StyleResolver` in `service/src/agent/style/mod.rs`.
2. `configs/workflows/*.yml` — workflow-specific prompts and messages.
3. Hardcoded strings in agent code (rare; usually refactor to config).

`/compound` captures voice principles here. Devs propagate the concise rule to the appropriate config in a follow-up PR with human review — voice changes ship to production, so they get eyes on every diff.

## Principles

Populated by `/compound` as patterns accrue. Starter examples:

### Tone
- Helpful without being chatty.
- Specific error descriptions, not "Something went wrong".
- Ask one clarifying question at a time, not a list.

### Structure
- Lead with the answer; offer details on request ("Would you like more details?").
- Use the user's own words when echoing back their request.
- Confirmations name the thing confirmed, not just "Done".

## Captured Rules

Dated entries appended by `/compound` after each PRD that touched bot-facing copy.

---
```

---

## Notes and iter-2 backlog

### Blocked on: `prd-recall-scores-and-supersession.md` (task-mgr repo)

When that PRD ships, **update `/compound` iter 2** to replace iter-1 fallbacks with first-class primitives:

1. **Step 2.6 graduation signal** — drop the "≥50% confidence" phrasing (scores are intentionally non-normalized per that PRD's Non-Goal #1). Replace with:
   ```bash
   # Recall excludes superseded by default in the new task-mgr; count of ≥12 is the graduation signal.
   task-mgr --format json recall --query "<learning title>" --limit 30 | jq '.learnings | length'
   ```
2. **Step 2.6 supersession flow** — replace the "learn-tag if supported, else human follow-up" fallback with:
   ```bash
   POINTER_ID=$(task-mgr --format json learn \
     --outcome pattern \
     --title "Use agent <name> for <topic>" \
     --content "..." \
     --tags "<original-tags>,graduated" \
     --confidence high | jq -r '.id')

   for src_id in $MATCHED_IDS; do
     task-mgr edit-learning $POINTER_ID --supersedes $src_id
   done
   ```
   (Each `edit-learning --supersedes` call inserts one row into `learning_supersessions`. Transitive chains handled by the `NOT IN` subquery — no chain-resolution logic needed.)
3. **Verify step** — confirm filtering works:
   ```bash
   # Should return 1 (pointer only)
   task-mgr --format json recall --query "<topic>" --limit 30 | jq '.learnings | length'
   # Should return 13 (pointer + 12 sources)
   task-mgr --format json recall --query "<topic>" --include-superseded --limit 30 | jq '.learnings | length'
   ```
4. **Drop the `learn-tag` hypothetical** — that PRD's Non-Goals explicitly reject a dedicated subcommand because `edit-learning --add-tags` already covers tag management. With first-class `--supersedes`, no tag dance is needed anyway.

### Not blocked (iter 2 can tackle in parallel)

- **Cross-project learnings initialization**: a `task-mgr` mechanism that collects `scope:generalizable` learnings from sibling projects to seed a new project's learnings DB. Requires first tagging existing learnings with `scope:generalizable` vs `scope:project-specific`. Separate PRD when appetite permits.
- **Concrete YAML-patch generation for copy voice**: instead of just prose captures in `copy-voice.md`, emit a `.patch` against `configs/base.yml` that a human can apply. Reduces propagation friction.
- **Changelog rollup generator**: `docs/change_logs/CHANGELOG_<YYYY-MM-DD>.md` daily files accumulate; a rollup script producing weekly/monthly summaries would help the human audience.

## Error handling

- PRD filename not given → prompt user.
- Task JSON missing → tell user the PRD hasn't been through `/tasks` yet.
- Progress file missing → tell user the loop hasn't run.
- Code review not in progress log → tell user to run `/review-loop` first.
- Any verify failure → report but do not retry automatically; the human decides.
