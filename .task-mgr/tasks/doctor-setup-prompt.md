# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Enhanced Doctor Command — Claude Settings Audit & Auto-Fix** for **task-mgr**.

## Problem Statement

Users repeatedly hit permission denials, missing skills, and misconfigured settings when running task-mgr loops. The root causes are: `defaultMode: "default"` in settings.json conflicting with `--allowedTools`, missing `.task-mgr/config.json`, missing skills in `~/.claude/commands/`, and `guard-destructive.sh` hook without loop bypass. These issues cascade into poisoned learnings and wasted iterations. The fix: extend `task-mgr doctor` with a `--setup` flag that audits Claude Code configuration, suggests fixes, and auto-repairs what it can.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Internalize quality targets** — Read `qualityDimensions` and define what "done well" looks like for THIS task
2. **Map edge cases to implementation plan** — Read `edgeCases`/`invariants`/`failureModes`; for each, decide HOW it will be handled before coding
3. **Choose your approach** — State assumptions, consider 2-3 approaches with tradeoffs, pick the best, document in progress file
4. **After coding, self-critique** — "Does this satisfy every qualityDimensions constraint? Every edge case? Is it idiomatic and efficient?" — revise before moving on

---

## Priority Philosophy

1. **PLAN** - Anticipate edge cases. Tests verify boundaries work correctly
2. **PHASE 2 FOUNDATION** - Extensible check registry so new checks can be added as functions without changing the orchestrator
3. **FUNCTIONING CODE** - Pragmatic, reliable code that works according to plan
4. **CORRECTNESS** - Code compiles, type-checks, all tests pass deterministically
5. **CODE QUALITY** - Clean code, good patterns, no warnings
6. **POLISH** - Documentation, formatting, minor improvements

**Prohibited outcomes:**
- Auto-fix that modifies `~/.claude/settings.json` without explicit user confirmation
- False positive checks that waste user time on correct configurations
- Error messages that don't include a copy-pasteable fix command
- Tests that only assert "no crash" or check type without verifying content

---

## Task Files (IMPORTANT)

| File | Purpose |
| --- | --- |
| `tasks/doctor-setup.json` | **Task list** - Read tasks, mark complete, add new tasks |
| `tasks/doctor-setup-prompt.md` | This prompt file (read-only) |
| `tasks/progress-{TASK_PREFIX}.txt` | Progress log (create if missing) |

---

## Your Task

1. Read the PRD at `tasks/doctor-setup.json`
2. Read progress log (create if missing)
3. Read `CLAUDE.md` for project patterns
4. Verify you're on branch `feat/doctor-setup`
5. **Select the best task** using Smart Task Selection
6. Pre-implementation review, implement, self-critique, quality checks
7. Commit and output `<completed>FULL-STORY-ID</completed>`

---

## Smart Task Selection

1. Filter eligible: `passes: false` AND all `dependsOn` complete
2. Check synergy with previous task
3. Check file overlap
4. Tie-breaker: lowest priority number

---

## Quality Checks (REQUIRED)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

---

## Reference Code

### Existing doctor command pattern (src/commands/doctor/mod.rs):
```rust
pub fn doctor(
    conn: &Connection,
    auto_fix: bool,
    dry_run: bool,
    decay_threshold: i64,
    reconcile_git: bool,
    dir: &Path,
) -> TaskMgrResult<DoctorResult> {
    let mut issues = Vec::new();
    let mut fixed = Vec::new();
    // ... run checks, collect issues, optionally fix ...
}
```

### Existing output types (src/commands/doctor/output.rs):
```rust
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum IssueType {
    StaleInProgressTask,
    ActiveRunWithoutEnd,
    OrphanedRelationship,
    DecayWarning,
    GitReconciliation,
}

pub struct Issue {
    pub issue_type: IssueType,
    pub entity_id: String,
    pub description: String,
}
```

### Existing CLI pattern (src/cli/commands.rs):
```rust
Doctor {
    #[arg(long = "auto-fix", default_value_t = false)]
    auto_fix: bool,
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,
    // ... add --setup here
}
```

### Existing skills check (src/loop_engine/engine.rs):
```rust
const EXPECTED_GLOBAL_SKILLS: &[&str] = &[
    "tm-apply", "tm-learn", "tm-recall",
    "tm-invalidate", "tm-status", "tm-next",
];
// check_global_skills() checks ~/.claude/commands/ for these
```

### CODING_ALLOWED_TOOLS (src/loop_engine/config.rs:161):
The constant contains all tools passed via --allowedTools. Import and use for deny-conflict checking.

### Settings.json structure to parse:
```json
{
  "permissions": {
    "allow": ["Bash(tool:*)"],
    "deny": ["Bash(tool:*)"],
    "ask": ["Bash(tool:*)"],
    "defaultMode": "default|auto|acceptEdits|dontAsk|bypassPermissions"
  },
  "hooks": {
    "PreToolUse": [{
      "matcher": "Bash",
      "hooks": [{"type": "command", "command": "/path/to/hook.sh"}]
    }]
  }
}
```

Parse with `serde_json::Value` — don't create a full typed struct since the format may change.

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **NEVER auto-modify ~/.claude/settings.json** — only print suggestions
- **Backup before modifying hooks** — create .bak file
