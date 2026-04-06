# PRD: Enhanced Doctor Command — Claude Settings Audit & Auto-Fix

**Type**: Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-03-22
**Status**: Draft

---

## 1. Overview

### Problem Statement

Users repeatedly hit permission denials, missing skills, and misconfigured settings when running task-mgr loops. The root causes are:
- `~/.claude/settings.json` with `defaultMode: "default"` conflicting with `--allowedTools`
- Missing `.task-mgr/config.json` with project-specific tool allowlists
- Missing `~/.claude/commands/` skill files
- `guard-destructive.sh` hook without loop bypass support
- Missing or incomplete `CLAUDE.md` project notes

These issues cause cascading failures: permission denials → poisoned learnings → agents that preemptively skip running commands. The fix should detect these issues early and offer to repair them.

### Background

The existing `doctor` command checks DB health (stale tasks, orphaned runs, etc.). This enhancement extends it with a new `--setup` or standalone audit mode that checks Claude Code configuration. The existing `check_global_skills` function in `engine.rs` already checks for missing skills at loop start — this work consolidates and extends that pattern.

Key learning: `.claude/commands/` and `.claude/agents/` are write-protected in dontAsk mode (learnings #1019, #1024, #1026). Auto-fix for skills MUST happen via the CLI `doctor` command, not during loop iterations.

---

## 2. Goals

### Primary Goals

- [ ] `task-mgr doctor --setup` audits global + project Claude settings and reports issues with fix suggestions
- [ ] Auto-fix mode (`--auto-fix`) repairs what it can: install skills, generate config, fix hooks
- [ ] Auto-check at loop start (new task lists only, not resume) warns about critical issues before wasting iterations
- [ ] JSON output mode (`--json`) for machine-readable diagnostics

### Success Metrics

- Running `task-mgr doctor --setup` in a fresh project reports all missing configuration
- Running `task-mgr doctor --setup --auto-fix` installs missing skills and generates config files
- Starting a loop on a new task list with misconfigured settings prints actionable warnings before first iteration
- Zero false positives on a correctly configured project

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Must never modify `~/.claude/settings.json` without explicit user confirmation — this is the user's Claude configuration
- Must correctly detect `defaultMode: "default"` as problematic even when other settings are fine
- Must detect hook scripts that lack the `LOOP_ALLOW_DESTRUCTIVE` bypass
- Auto-fix must be idempotent — running twice produces the same result

### Performance Requirements

- Audit must complete in <500ms (reads local files only, no network)
- Loop start auto-check must add <100ms to startup time

### Style Requirements

- Follow existing `doctor` command patterns (IssueType enum, DoctorResult struct)
- Warnings should include copy-pasteable fix commands, not just descriptions
- Color-coded output: red for blockers, yellow for warnings, green for passing checks

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| `~/.claude/settings.json` doesn't exist | New Claude Code installation | Report as info (not error), suggest creating with sane defaults |
| `guard-destructive.sh` has custom modifications | User may have added their own checks | Only check for `LOOP_ALLOW_DESTRUCTIVE` bypass, don't overwrite custom logic |
| Skills source dir doesn't exist in repo | Not all projects have `.claude/commands/` | Skip skill installation, only warn about missing global skills |
| Settings.json has `deny` rules matching tools in CODING_ALLOWED_TOOLS | Direct conflict that blocks loops | Report as critical blocker with exact conflicting rules |
| Worktree missing `.claude/settings.local.json` | Project settings don't propagate to worktrees | Warn and suggest copying, or suggest putting in `.task-mgr/config.json` instead |
| Multiple projects with different tool needs | Global settings can't satisfy all projects | Recommend per-project `.task-mgr/config.json` over global settings changes |

---

## 3. User Stories

### US-001: Explicit Audit via CLI

**As a** developer setting up task-mgr for a new project
**I want** `task-mgr doctor --setup` to tell me what's misconfigured
**So that** I can fix all issues before wasting loop iterations on permission errors

**Acceptance Criteria:**

- [ ] Checks global `~/.claude/settings.json` for: defaultMode, deny rules conflicting with CODING_ALLOWED_TOOLS, missing common tools in allow list
- [ ] Checks `guard-destructive.sh` hook for LOOP_ALLOW_DESTRUCTIVE bypass
- [ ] Checks `~/.claude/commands/` for expected skills
- [ ] Checks `.task-mgr/config.json` exists with additionalAllowedTools
- [ ] Checks project `CLAUDE.md` exists
- [ ] Each issue includes severity (blocker/warning/info), description, and copy-pasteable fix
- [ ] Summary at end: X blockers, Y warnings, Z passing

### US-002: Auto-Fix Mode

**As a** developer who wants quick setup
**I want** `task-mgr doctor --setup --auto-fix` to fix what it can automatically
**So that** I don't have to manually copy skills and create config files

**Acceptance Criteria:**

- [ ] Copies missing skills from repo `.claude/commands/` to `~/.claude/commands/`
- [ ] Generates `.task-mgr/config.json` with project-appropriate tools if missing
- [ ] Patches `guard-destructive.sh` to add LOOP_ALLOW_DESTRUCTIVE bypass if missing
- [ ] Generates template `CLAUDE.md` if missing
- [ ] NEVER auto-modifies `~/.claude/settings.json` — only prints suggestions
- [ ] Reports what was fixed vs what needs manual action

### US-003: Auto-Check at Loop Start

**As a** developer starting a new task list
**I want** the loop to warn me about critical configuration issues before the first iteration
**So that** I don't waste iterations on preventable permission denials

**Acceptance Criteria:**

- [ ] Runs subset of checks (blockers only) when starting a new task list (not resuming)
- [ ] Prints yellow warning banner with top issues
- [ ] Does not block loop start — just warns
- [ ] Skips check on resume (loop continues from where it left off)

### US-004: JSON Output

**As a** developer building tooling around task-mgr
**I want** `task-mgr doctor --setup --json` to output machine-readable diagnostics
**So that** I can integrate with CI or custom setup scripts

**Acceptance Criteria:**

- [ ] JSON output includes all checks with: name, status (pass/warn/fail), message, fix_command
- [ ] Follows existing `--format json` patterns in the codebase

---

## 4. Functional Requirements

### FR-001: Global Settings Audit

Check `~/.claude/settings.json`:

- `defaultMode` is not `"default"` (blocker — causes permission denials in loops)
- No `deny` rules that match tools in `CODING_ALLOWED_TOOLS` (blocker)
- `ask` rules don't include non-destructive tools (warning)
- Hooks section: `guard-destructive.sh` exists and has `LOOP_ALLOW_DESTRUCTIVE` bypass (warning)

**Validation:** Test with settings.json containing `defaultMode: "default"` reports blocker.

### FR-002: Project Config Audit

Check `.task-mgr/config.json`:

- File exists (warning if missing)
- `additionalAllowedTools` includes project-specific tools detected from the project (e.g., `protoc` if `.proto` files exist, `docker` if `Dockerfile` exists) (info)

**Validation:** Test in project without config.json reports warning with suggested content.

### FR-003: Skills Audit

Check `~/.claude/commands/`:

- All `EXPECTED_GLOBAL_SKILLS` have corresponding `.md` files (warning)
- Compare file sizes/hashes with source copies in repo `.claude/commands/` if they exist (info — stale skills)

**Validation:** Test with missing skill reports warning with installation command.

### FR-004: CLAUDE.md Audit

Check project `CLAUDE.md`:

- File exists in project root (info if missing)
- Contains database location reference (info if missing)
- Contains worktree information if worktrees are configured (info)

**Validation:** Test without CLAUDE.md suggests generating a template.

### FR-005: Auto-Fix Engine

When `--auto-fix` is passed:

- Install missing skills by copying from source
- Generate `.task-mgr/config.json` with auto-detected tools
- Patch `guard-destructive.sh` hook with LOOP_ALLOW_DESTRUCTIVE bypass
- Generate template CLAUDE.md
- Print `~/.claude/settings.json` suggestions (manual action required)

**Validation:** Running auto-fix then re-running audit shows all fixed issues now pass.

### FR-006: Loop Start Pre-Check

At loop startup, when starting a new task list (not resuming):

- Run blocker-level checks only (defaultMode, deny conflicts)
- Print yellow warning banner if issues found
- Continue loop regardless (non-blocking)

**Validation:** Starting loop with `defaultMode: "default"` prints warning.

---

## 5. Non-Goals (Out of Scope)

- **Auto-modifying `~/.claude/settings.json`** — Too risky; this is the user's Claude configuration. Only print suggestions.
- **Checking Claude Code version** — Not our responsibility; different update cadence.
- **Network checks** — No API key validation, no connectivity tests.
- **Per-worktree settings audit** — Could be added later but adds complexity for marginal value.
- **Custom skill generation** — Just copy existing skills; don't generate new ones.

---

## 6. Technical Considerations

### Affected Components

- `src/commands/doctor/mod.rs` — Add `--setup` flag, new audit orchestrator
- `src/commands/doctor/checks.rs` — New check functions for settings, skills, hooks, CLAUDE.md
- `src/commands/doctor/output.rs` — New IssueType variants, SetupResult struct
- `src/commands/doctor/fixes.rs` — New auto-fix functions (install skills, generate config, patch hook)
- `src/cli/commands.rs` — Add `--setup` flag to Doctor command
- `src/loop_engine/engine.rs` — Add pre-check call at loop start (new task lists only)
- `src/loop_engine/config.rs` — Extract `CODING_ALLOWED_TOOLS` check logic for reuse

### Dependencies

- `serde_json` — Parse settings.json (already a dependency)
- `dirs` or `home_dir` — Find `~/.claude/` (check what's already used)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| Extend existing `doctor` command with `--setup` flag | Reuses existing infrastructure (IssueType, DoctorResult, CLI wiring); single entry point for health checks | Doctor currently focused on DB health; mixing concerns | **Preferred** — users already know `doctor` |
| New standalone `setup` command | Clean separation; dedicated UX | Duplicates output/fix infrastructure; another command to learn | Alternative |
| Separate `audit` subcommand under `doctor` (`doctor audit`) | Clean namespace; extensible | Adds nesting complexity | Rejected — too much nesting |

**Selected Approach**: Extend `doctor` with `--setup` flag. When `--setup` is passed, run Claude settings checks instead of (or in addition to) DB health checks. When neither `--setup` nor DB-specific flags are passed, run both.

**Phase 2 Foundation Check**: The check infrastructure (check registry, severity levels, auto-fix protocol) should be designed for extensibility. New checks (e.g., MCP server config, model availability) can be added as functions without changing the orchestrator. This costs minimal extra effort now but makes the system composable for future checks.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Auto-fix breaks user's custom hook | High — blocks all interactive Claude use | Low — we only add bypass, don't modify existing checks | Check for existing `LOOP_ALLOW_DESTRUCTIVE` before patching; create backup |
| False positive on settings.json | Medium — user wastes time "fixing" correct config | Medium — settings interactions are complex | Only flag known-bad patterns; include "why" explanation with each warning |
| Skills source files out of date | Low — stale skills cause confusing behavior | Medium — skills change across releases | Hash comparison with source; warn if different |

### Security Considerations

- Auto-fix writes to `~/.claude/` — must not follow symlinks or write outside expected paths
- Hook patching must preserve existing security checks — only ADD the bypass, don't remove anything
- Never expose contents of settings.json in logs (could contain API key helpers)

### Public Contracts

#### New Interfaces

| Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `audit_setup(dir, global_only)` | `(dir: &Path, global_only: bool) -> TaskMgrResult<SetupAuditResult>` | `SetupAuditResult { checks: Vec<SetupCheck> }` | `TaskMgrError` | Reads files only |
| `fix_setup_issues(dir, checks)` | `(dir: &Path, checks: &[SetupCheck]) -> TaskMgrResult<Vec<SetupFix>>` | `Vec<SetupFix>` | `TaskMgrError` | Writes files (skills, config, hook, CLAUDE.md) |
| `pre_check_loop_setup(dir)` | `(dir: &Path) -> Vec<SetupCheck>` | Blocker-level checks only | Never fails (logs and continues) | None |

#### New Types

```rust
pub enum SetupSeverity { Blocker, Warning, Info, Pass }

pub struct SetupCheck {
    pub name: String,
    pub category: SetupCategory,  // Permissions, Hooks, Skills, ProjectConfig, Documentation
    pub severity: SetupSeverity,
    pub message: String,
    pub fix_command: Option<String>,  // Copy-pasteable fix
    pub auto_fixable: bool,
}

pub struct SetupAuditResult {
    pub checks: Vec<SetupCheck>,
    pub blockers: usize,
    pub warnings: usize,
    pub passing: usize,
}
```

### Data Flow Contracts

N/A — no cross-module data access. Checks read files and return results; no complex data paths.

### Inversion Checklist

- [x] All callers identified? Yes — doctor CLI + loop startup
- [x] What breaks if check is wrong? False positive wastes user time; false negative wastes loop iterations
- [x] What if settings.json format changes? Parse gracefully; unknown fields are OK
- [x] What if hook format changes? Grep for bypass pattern; don't assume line numbers

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `CLAUDE.md` | Update | Add note about `task-mgr doctor --setup` for troubleshooting |

---

## 7. Open Questions

- [x] Should auto-fix back up files before modifying? **Yes** — create `.bak` for hooks
- [ ] Should the loop start pre-check be configurable (skip with env var)?
- [ ] What project-specific tools should be auto-detected? (protoc for .proto, docker for Dockerfile, python for .py, etc.)

---

## Appendix

### Related Learnings

- **#1019, #1024, #1026**: `.claude/commands/` write-protected in dontAsk mode — skills MUST be installed via CLI, not loop
- **#1013**: `~/.claude/agents/` also write-blocked in autonomous mode
- **#1014**: Tasks modifying Claude config files should be flagged as non-automatable

### Existing Infrastructure to Reuse

- `src/commands/doctor/` — IssueType enum, DoctorResult, output formatting, fix infrastructure
- `src/loop_engine/engine.rs:770-823` — `EXPECTED_GLOBAL_SKILLS` and `check_global_skills()`
- `src/loop_engine/config.rs:161` — `CODING_ALLOWED_TOOLS` constant
- `src/loop_engine/project_config.rs` — `read_project_config()` and `ProjectConfig` struct

### Check Registry (Implementation Guide)

| Check | Category | Severity | Auto-Fixable | What It Checks |
| --- | --- | --- | --- | --- |
| `defaultMode` | Permissions | Blocker | No (suggest) | settings.json `defaultMode` != "default" |
| `deny_conflicts` | Permissions | Blocker | No (suggest) | No deny rules matching CODING_ALLOWED_TOOLS |
| `ask_conflicts` | Permissions | Warning | No (suggest) | No ask rules for non-destructive tools |
| `hook_bypass` | Hooks | Warning | Yes | guard-destructive.sh has LOOP_ALLOW_DESTRUCTIVE |
| `hook_exists` | Hooks | Info | No | guard-destructive.sh exists at expected path |
| `skills_installed` | Skills | Warning | Yes | All EXPECTED_GLOBAL_SKILLS present |
| `skills_current` | Skills | Info | Yes | Skills match source hashes |
| `project_config` | ProjectConfig | Warning | Yes | .task-mgr/config.json exists |
| `project_tools` | ProjectConfig | Info | Yes | Auto-detected tools in config |
| `claude_md` | Documentation | Info | Yes | CLAUDE.md exists with key sections |
| `worktree_settings` | Permissions | Warning | No (suggest) | Worktree has .claude/settings.local.json |
