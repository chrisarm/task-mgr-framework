---
name: review-loop
description: "Review completed autonomous loop code for correctness, security, and PRD alignment. Backward-looking: 'was this built correctly?' Does NOT update documentation — that's /compound's job (forward-looking capture). Leads to /compound when the review is clean. Use when the user says 'review the loop for X', 'review loop results', 'code review the completed PRD', or variations. Triggers on /review-loop slash command. Takes a PRD markdown filename as argument."
user_invocable: true
---

# /review-loop — Review Completed Loop Code

Backward-looking review of code produced by a completed autonomous loop run. Checks quality, security, and PRD coherence.

**This skill does NOT update docs or downstream PRDs** — forward-looking capture moved to `/compound`.

**Followed with `/compound`**: `/review-loop` answers "was this built correctly?"; `/compound` answers "what should future PRDs inherit from this one?" Run `/review-loop` first; it auto-chains to `/compound` when the review is clean.

## Usage

```
/review-loop tasks/prd-<feature>.md
```

## What This Does

1. Finds the loop's worktree.
2. Runs code review (via `standard-code-reviewer` or `rust-python-code-reviewer` subagent).
3. Assesses PRD coherence **inline** (no second subagent — main agent keeps the code reviewer's findings in working context).
4. Presents a consolidated report.
5. **Auto-chains to `/compound`** unless the review found critical findings.

## Where commits and edits land — READ THIS FIRST

**Every file edit, write, and commit produced by this skill or its chained `/compound` invocation MUST land in the PRD's worktree (`branchName` from the JSON), NOT in the cwd you launched from (which is usually the main repo on the `main` branch).**

The cwd you launch `/review-loop` from is almost always the main repo checkout on `main`. `main` is push-protected via repo rules ("changes must come through a pull request"). A commit landing on local `main` becomes homeless: it can't be pushed, and the work has to be cherry-picked onto the PRD branch later (with all the disk/conflict friction that implies). Captures and inline fixes belong on the worktree branch so they ship with the PRD's PR.

The two repos (main + worktree) share an identical relative file layout. If you Edit `/.../codebase/CLAUDE.md` instead of `/.../codebase-worktrees/<branch>/CLAUDE.md`, the tool will succeed and the change will go to the wrong branch silently. There is no warning. Path discipline is the only defense.

**Mechanics — do all four:**

1. **Resolve `WORKTREE=<worktree-path>` once (Step 1 below) and reuse it throughout** — single source of truth for where everything goes.
2. **`cd "$WORKTREE"` immediately after resolving it**, so cwd-relative commands (`git status`, `cargo test`, etc.) naturally target the right repo. Stay there for the rest of the skill.
3. **Every git operation must use `git -C "$WORKTREE" ...`** even after the `cd`. Belt-and-suspenders — survives accidental `cd` elsewhere.
4. **Every Edit/Write tool call must use an absolute path that starts with `$WORKTREE/`.** Before invoking Edit or Write, mentally check the `file_path` argument: does it begin with the worktree path you resolved in Step 1? If it begins with the main-repo path, you are about to commit to the wrong branch. Stop and re-target.

**When chaining to `/compound`**, pass `$WORKTREE` so it commits there too. (See `/compound` skill — it reads the same `branchName` from the JSON.)

This applies to EVERY write, no matter how small: doc typos flagged by the reviewer, a `CLAUDE.md` line added during coherence spot-checks, a learning captured by `/compound`. If a write touches the filesystem during this skill, it touches `$WORKTREE`.

## Instructions

### Step 1: Resolve the PRD and Worktree

Given the PRD filename argument (e.g., `tasks/prd-phase1-demo-loop.md`):

1. **Read the PRD** to understand what was supposed to be built.
2. **Derive the task JSON path** — same basename, `.json` extension.
3. **Extract `branchName`** via `jq -r '.branchName' tasks/<feature>.json`. Do NOT Read the full JSON.
4. **Find the worktree** — parent-directory-sibling `-worktrees/` directory, with `/` in the branch name replaced by `-`:
   ```
   # If cwd is /home/chris/Dropbox/startat0/DeskMaiT/codebase
   # Look in /home/chris/Dropbox/startat0/DeskMaiT/codebase-worktrees/feat-phase1-harmony-console
   ```
   Fallback: `git worktree list`.
5. **Verify the worktree has commits beyond main**:
   ```bash
   cd <worktree-path> && git log --oneline main..HEAD | head -5
   ```
6. **Capture the worktree path and `cd` into it:**
   ```bash
   WORKTREE=<worktree-path>
   cd "$WORKTREE"
   ```
   - `$WORKTREE` is the single source of truth for "where everything goes" — every later `git add`/`git commit` and every Edit/Write `file_path` is rooted there.
   - The `cd` makes cwd-relative commands (`git status`, `cargo test`, ad-hoc `grep`) target the worktree by default. Stay in `$WORKTREE` for the rest of the skill; do not `cd` back to the main repo.
   - Even after `cd`, prefer `git -C "$WORKTREE" ...` for git ops so an accidental `cd` elsewhere doesn't silently move commits to `main`.

If the worktree can't be found, tell the user and ask for the path.

### Step 2: Gather Context

In the worktree:

```bash
git -C $WORKTREE diff --stat main..HEAD          # file list + line counts
git -C $WORKTREE log --oneline main..HEAD        # commit history
```

Pull quality dimensions from the task JSON:

```bash
jq '.userStories[].qualityDimensions' tasks/<feature>.json
```

Pull data flow contracts from the prompt file (if any):

```bash
grep -n -A 30 '## Data Flow Contracts' tasks/<feature>-prompt.md
```

### Step 3: Code Review (subagent)

Launch a single code-review subagent — pick based on the diff's dominant language:

- Rust-heavy or mixed Rust/Python → `rust-python-code-reviewer`
- Other → `standard-code-reviewer`

Prompt the subagent with:

- The worktree path (`$WORKTREE`).
- The list of changed files from `git diff --stat`.
- The `qualityDimensions` pulled in Step 2 as review criteria.
- Any security considerations called out in the PRD.
- Instruction to produce a structured report with **critical / high / medium / low** findings, each with `file:line` references.

The subagent reads the files and returns the finding list. **Do NOT spawn a second agent in parallel** — the coherence check below runs inline.

### Step 4: Coherence Check (inline, main agent)

After the code-review subagent returns, the main agent (you) does the coherence check using the findings + your already-loaded PRD context:

- Spot-check the implementation in the worktree against PRD acceptance criteria and data-flow contracts — `grep` / `Read` narrowly inside `$WORKTREE`, do NOT re-read the full worktree.
- Flag deviations: missing features, unexpected additions, contract violations (e.g., struct field name drift across module boundaries).
- Note whether each PRD user story appears to be satisfied based on the code reviewer's findings + your spot-checks.

This is NOT a full second code review — it's a targeted coherence pass that leverages the subagent's findings.

**If you make any inline fixes during this step** (a doc typo, a one-line correction the reviewer surfaced as trivial), the Edit/Write `file_path` MUST begin with `$WORKTREE/`. Commit via `git -C "$WORKTREE" ...`. Same rule as the top-of-skill mechanics — no exceptions for "small" edits. If a fix is non-trivial, prefer spawning a `CODE-FIX` task (see Step 6 critical-findings branch) over patching it here.

### Step 5: Consolidated Report

Present to the user:

```markdown
## Loop Review: <PRD Title>

### Code Review Summary

- **Files reviewed**: N
- **Critical findings**: N
- **High findings**: N
- **Medium/Low findings**: N

<Critical and high findings with file:line references>

### Coherence Assessment

- **PRD alignment**: FULL / PARTIAL / DIVERGENT
- **Deviations found**: <list any gaps or unexpected changes>
- **Cross-PRD contract status**: <any type mismatches or API drift>

### Action Items

- <any issues that need manual attention>
```

### Step 6: Chain to `/compound`

Decision tree based on findings:

- **No critical findings** (only medium/low/informational) → **Recommend `/compound`** with the same PRD argument. Learnings are captured while context is fresh:

  ```
  Skill(skill="compound", args="tasks/prd-<feature>.md")
  ```

  Tell the user: "Review clean. Run `/compound` to capture forward-looking learnings."

  When `/compound` runs, it commits its captures to `$WORKTREE` (the PRD's worktree branch), per the rules at the top of this skill.

- **High findings, no critical** → **ask the user**:

  > High findings present but none critical. Run `/compound` now to capture what was learned, or address the findings first? [compound / wait]

  If "compound", invoke as above. If "wait", stop and print: "Address the high findings, re-run `/review-loop`, to lead up to `/compound`."

- **Any critical findings** → **do NOT invoke `/compound`**. Print:
  > Critical findings above block merge. Address them (typically by spawning `CODE-FIX` tasks via `task-mgr add --stdin --depended-on-by MILESTONE-FINAL` and re-running the loop), then re-run `/review-loop`. `/compound` best run after the review is clean — capturing learnings from unresolved critical issues risks baking wrong wisdom into CLAUDE.md.

**Rationale for the gate**: `/compound` writes to CLAUDE.md and the learnings DB. Capturing "wisdom" from a broken implementation poisons future PRDs. Clean review first, then compound.

## Error Handling

- **Worktree not found** → ask the user for the path.
- **No commits in worktree** → warn that the loop may not have completed; do not proceed.
- **`tasks/<feature>.json` missing** → tell the user the PRD hasn't been through `/tasks`.
- **Code-review subagent timeout** → report what completed; recommend adding `/compound` to the fix plan or follow-up tasks.
- **Push to main rejected ("changes must come through a PR")** → you committed to the wrong branch. The compound/fix commit landed on local `main` instead of `$WORKTREE`. Recover by: (a) cherry-pick the commit onto the worktree branch via `git -C "$WORKTREE" cherry-pick <hash>`, (b) push the worktree branch, (c) reset local main via `git reset --keep origin/main`. Then audit: re-read the "Where commits and edits land" section above to make sure the next run uses `cd "$WORKTREE"` + `git -C "$WORKTREE"` + `$WORKTREE/`-prefixed Edit paths from the start.
- **Edit/Write targeted the main repo by mistake** (file_path started with the main-repo path, not `$WORKTREE/`) → the change is now on the wrong working tree. Recover by: (a) capture the diff via `git -C <main-repo> diff -- <path>`, (b) revert in the main repo (`git -C <main-repo> restore <path>`), (c) re-apply the same edit with `file_path = $WORKTREE/<path>`, (d) commit in the worktree.

## Notes

- The code reviewer reads files in the WORKTREE (where the loop ran).
- One subagent (code review) + one inline pass (coherence) — fewer agents, more context continuity.
- Handles Rust, Python, and mixed codebases (pick the right reviewer variant).
