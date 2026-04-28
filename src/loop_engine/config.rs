/// Multiplier for auto-calculating max iterations from task count.
///
/// Provides headroom for reorders, retries, multi-step tasks, and dynamically
/// spawned tasks (CODE-FIX, WIRE-FIX, REFACTOR, IMPL-FIX from review steps).
/// 1.75x gives ~75% headroom so fix/refactor tasks added mid-run don't exhaust
/// the iteration budget.
const ITERATION_HEADROOM_MULTIPLIER: f64 = 1.75;

/// Configuration for the autonomous agent loop engine.
///
/// Defaults are designed for typical usage. Use `from_env()` to override
/// via environment variables. Invalid values fall back to defaults.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum iterations before stopping (0 = auto-calculate from task count)
    pub max_iterations: usize,
    /// Usage API threshold percentage (0-100) to trigger wait-for-reset
    pub usage_threshold: u8,
    /// Maximum consecutive crashes before aborting the loop
    pub max_crashes: u8,
    /// Delay in seconds between iterations
    pub iteration_delay_secs: u64,
    /// Seconds to wait when usage check has no reset time available
    pub usage_fallback_wait: u64,
    /// Whether to check the usage API before iterations
    pub usage_check_enabled: bool,
    /// Auto-confirm all prompts (non-interactive mode)
    pub yes_mode: bool,
    /// Optional time budget in hours
    pub hours: Option<f64>,
    /// Verbose output mode
    pub verbose: bool,
    /// Whether to use git worktrees instead of branch checkout
    ///
    /// When true (default), the loop creates a git worktree for the PRD branch
    /// instead of checking out the branch directly. This avoids "would be
    /// overwritten" errors when there are uncommitted changes.
    pub use_worktrees: bool,
    /// Number of recent commits to scan for task completion within the loop.
    ///
    /// A tight window (default 7) prevents stale commits from falsely completing
    /// tasks in the current iteration.
    pub git_scan_depth: usize,
    /// Number of recent commits to scan in external repo reconciliation.
    ///
    /// A broader window (default 50) ensures legitimate external completions
    /// aren't missed across prior runs.
    pub external_git_scan_depth: usize,
    /// Whether to remove the worktree on loop exit.
    ///
    /// When true (set by --cleanup-worktree), the worktree is removed after
    /// the loop finishes. Only applies when `use_worktrees` is true and the
    /// loop is running in a worktree. Dirty worktrees are warned but not forced.
    pub cleanup_worktree: bool,
    /// Number of tasks to execute in parallel per wave (1-3, default 2).
    ///
    /// Set via `--parallel N` CLI flag or `LOOP_PARALLEL` env var. Set to 1
    /// to force sequential execution. Values outside 1-3 are rejected.
    pub parallel_slots: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 0,
            usage_threshold: 92,
            max_crashes: 3,
            iteration_delay_secs: 2,
            usage_fallback_wait: 300,
            usage_check_enabled: true,
            yes_mode: false,
            hours: None,
            verbose: false,
            use_worktrees: true,
            git_scan_depth: 7,
            external_git_scan_depth: 50,
            cleanup_worktree: false,
            parallel_slots: 2,
        }
    }
}

impl LoopConfig {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// Calls `dotenvy::dotenv().ok()` first to load `.env` if present (no error
    /// if missing). Then reads these env vars:
    ///
    /// - `LOOP_MAX_ITERATIONS` → `max_iterations` (usize)
    /// - `LOOP_USAGE_THRESHOLD` → `usage_threshold` (u8, 0-100)
    /// - `LOOP_MAX_CRASHES` → `max_crashes` (u8)
    /// - `LOOP_ITERATION_DELAY_SECS` → `iteration_delay_secs` (u64)
    /// - `LOOP_USAGE_FALLBACK_WAIT` → `usage_fallback_wait` (u64)
    /// - `LOOP_USAGE_CHECK_ENABLED` → `usage_check_enabled` (bool: "true"/"1"/"yes")
    /// - `LOOP_GIT_SCAN_DEPTH` → `git_scan_depth` (usize, default 7)
    /// - `LOOP_EXTERNAL_GIT_SCAN_DEPTH` → `external_git_scan_depth` (usize, default 50)
    /// - `LOOP_PARALLEL` → `parallel_slots` (usize, 1-3, default 2)
    ///
    /// Invalid values are silently ignored (defaults used).
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();

        let defaults = Self::default();

        Self {
            max_iterations: parse_env("LOOP_MAX_ITERATIONS").unwrap_or(defaults.max_iterations),
            usage_threshold: parse_env("LOOP_USAGE_THRESHOLD").unwrap_or(defaults.usage_threshold),
            max_crashes: parse_env("LOOP_MAX_CRASHES").unwrap_or(defaults.max_crashes),
            iteration_delay_secs: parse_env("LOOP_ITERATION_DELAY_SECS")
                .unwrap_or(defaults.iteration_delay_secs),
            usage_fallback_wait: parse_env("LOOP_USAGE_FALLBACK_WAIT")
                .unwrap_or(defaults.usage_fallback_wait),
            usage_check_enabled: parse_env_bool("LOOP_USAGE_CHECK_ENABLED")
                .unwrap_or(defaults.usage_check_enabled),
            yes_mode: defaults.yes_mode,
            hours: defaults.hours,
            verbose: defaults.verbose,
            use_worktrees: defaults.use_worktrees,
            git_scan_depth: parse_env("LOOP_GIT_SCAN_DEPTH").unwrap_or(defaults.git_scan_depth),
            external_git_scan_depth: parse_env("LOOP_EXTERNAL_GIT_SCAN_DEPTH")
                .unwrap_or(defaults.external_git_scan_depth),
            cleanup_worktree: defaults.cleanup_worktree,
            parallel_slots: parse_env::<usize>("LOOP_PARALLEL")
                .filter(|&n| (1..=3).contains(&n))
                .unwrap_or(defaults.parallel_slots),
        }
    }
}

/// Parse a string value into a type that implements `FromStr`.
/// Returns `None` if parsing fails.
fn parse_value<T: std::str::FromStr>(value: &str) -> Option<T> {
    value.parse().ok()
}

/// Parse an environment variable into a type that implements `FromStr`.
/// Returns `None` if the var is missing or fails to parse.
fn parse_env<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| parse_value(&v))
}

/// Parse a string value as a boolean.
/// Accepts "true", "1", "yes" (case-insensitive) as true.
/// Accepts "false", "0", "no" (case-insensitive) as false.
/// Returns `None` if unrecognized.
pub(crate) fn parse_bool_value(value: &str) -> Option<bool> {
    match value.to_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Parse an environment variable as a boolean.
/// Returns `None` if the var is missing or unrecognized.
fn parse_env_bool(key: &str) -> Option<bool> {
    std::env::var(key).ok().and_then(|v| parse_bool_value(&v))
}

/// Tools allowed for the coding agent in scoped permission mode.
///
/// Covers cargo, git, task-mgr CLI, and file operations needed for autonomous
/// coding tasks. Derived from analysis of scripts/prompt.md.
///
/// # Security note
///
/// Scoped mode controls Claude CLI permission grants, not a sandbox.
/// Tools like `Bash(cat:*)` and `Bash(git:*)` permit arbitrary file
/// reads and remote pushes respectively. For stricter isolation, use a
/// container or VM.
pub const CODING_ALLOWED_TOOLS: &str = "Read,Edit,Write,WebFetch,WebSearch,NotebookEdit,Agent,LSP,Bash(cargo:*),Bash(git:*),Bash(task-mgr:*),Bash(mkdir:*),Bash(ls:*),Bash(wc:*),Bash(head:*),Bash(tail:*),Bash(cat:*),Bash(find:*),Bash(rg:*),Bash(sed:*),Bash(cd:*),Bash(ruff:*),Bash(mypy:*),Bash(uv:*),Bash(pytest:*),Bash(python:*),Bash(pip:*),Bash(npm:*),Bash(npx:*),Bash(node:*),Bash(bun:*),Bash(pnpm:*),Bash(yarn:*),Bash(make:*),Bash(grep:*),Bash(awk:*),Bash(sort:*),Bash(uniq:*),Bash(tr:*),Bash(cut:*),Bash(diff:*),Bash(touch:*),Bash(cp:*),Bash(mv:*),Bash(rm:*),Bash(chmod:*),Bash(echo:*),Bash(printf:*),Bash(tee:*),Bash(xargs:*),Bash(jq:*),Bash(yq:*),Bash(tree:*),Bash(which:*),Bash(command:*),Bash(pwd:*),Bash(realpath:*),Bash(dirname:*),Bash(basename:*),Bash(date:*),Bash(stat:*),Bash(env:*),Bash(rustup:*),Bash(mix:*),Bash(elixir:*),Bash(iex:*),Bash(hex:*),Bash(rebar3:*),Bash(shellcheck:*),Bash(shfmt:*),Bash(strings:*),Bash(file:*),Glob,Grep";

/// Tools to deny for the coding agent, passed via `--disallowedTools`.
///
/// Prevents the loop agent from directly editing or creating `.task-mgr/tasks/*.json`
/// files. The loop engine is the sole writer of PRD JSON; agents must use
/// `task-mgr add --stdin` or `<task-status>` tags instead.
///
/// # Research note
///
/// The Claude CLI does not support negative patterns within `--allowedTools`
/// (e.g. `Edit(!tasks/*.json)` is not a documented syntax). The separate
/// `--disallowedTools` flag is the correct mechanism for path-scoped denials.
///
/// `Read` on tasks/*.json is intentionally NOT denied — agents may read the PRD
/// for context. `Bash(task-mgr:*)` is intentionally NOT denied — it is the
/// approved replacement for JSON edits.
pub(crate) const TASKS_JSON_DISALLOWED_TOOLS: &str =
    "Edit(.task-mgr/tasks/*.json),Write(.task-mgr/tasks/*.json)";

/// Permission mode for Claude subprocess invocation.
///
/// Determines which permission flags are passed to `claude` when spawning a
/// subprocess. `Scoped` is the secure default; `Dangerous` is the legacy
/// escape hatch.
///
/// # Call sites
///
/// | Caller                           | Mode                                           |
/// |----------------------------------|------------------------------------------------|
/// | `engine::run_loop`               | `resolve_permission_mode(db_dir)` (env + config)|
/// | `curate::enrich`                 | `text_only()` — no tool access needed          |
/// | `curate::dedup`                  | `text_only()` — no tool access needed          |
/// | `learnings::ingestion`           | `text_only()` — no tool access needed          |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionMode {
    /// Legacy mode: passes `--dangerously-skip-permissions`.
    /// Enabled by `LOOP_PERMISSION_MODE=dangerous`.
    Dangerous,
    /// Scoped mode: passes `--permission-mode dontAsk [--allowedTools <tools>]`.
    /// Default when no env vars are set. Denies any tool not explicitly listed.
    /// `allowed_tools`: when `Some`, passed as `--allowedTools`; when `None`, omitted.
    Scoped { allowed_tools: Option<String> },
    /// Auto mode: passes `--permission-mode auto [--allowedTools <tools>]`.
    /// Enabled by `LOOP_PERMISSION_MODE=auto` or `LOOP_ENABLE_AUTO_MODE=true` (legacy).
    /// The `allowed_tools` list ensures common coding tools are pre-approved
    /// even in non-interactive (`--print`) mode where auto-mode cannot prompt.
    Auto { allowed_tools: Option<String> },
}

impl PermissionMode {
    /// Scoped mode with no tool restrictions, for text-only analysis tasks
    /// (learning extraction, enrichment, deduplication).
    pub fn text_only() -> Self {
        Self::Scoped {
            allowed_tools: None,
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dangerous => write!(f, "Dangerous (no tool restrictions)"),
            Self::Scoped {
                allowed_tools: None,
            } => write!(f, "Scoped (text-only, no tools)"),
            Self::Scoped {
                allowed_tools: Some(tools),
            } => {
                let count = tools.split(',').count();
                write!(f, "Scoped ({count} tools)")
            }
            Self::Auto {
                allowed_tools: None,
            } => write!(f, "Auto"),
            Self::Auto {
                allowed_tools: Some(tools),
            } => {
                let count = tools.split(',').count();
                write!(f, "Auto ({count} tools)")
            }
        }
    }
}

/// Resolve permission mode, merging project-specific tools if applicable.
///
/// Resolution order (highest to lowest priority):
/// 1. `LOOP_PERMISSION_MODE=dangerous` → `Dangerous` (no tool restrictions)
/// 2. `LOOP_PERMISSION_MODE=scoped` → `Scoped` with `CODING_ALLOWED_TOOLS` + project config
/// 3. `LOOP_PERMISSION_MODE=auto` → `Auto` with `CODING_ALLOWED_TOOLS` + project config
/// 4. `LOOP_ENABLE_AUTO_MODE=true` → `Auto` (legacy env var)
/// 5. `LOOP_ALLOWED_TOOLS=<tools>` → `Scoped` with those tools (NO project config merge)
/// 6. `.task-mgr/config.json` `permissionMode` field → corresponding variant
/// 7. Default → `Dangerous` (no permission prompts, all tools allowed)
///
/// Note: `Dangerous` is the default — opt back into `Scoped`/`Auto` via the
/// project config or `LOOP_PERMISSION_MODE` to enforce tool restrictions.
/// Both `scoped` and `auto` modes merge `CODING_ALLOWED_TOOLS` with
/// project-specific tools from `.task-mgr/config.json`. The difference is the
/// Claude CLI flag: `auto` passes `--permission-mode auto` (auto-approves
/// unlisted tools interactively), while `scoped` passes `--permission-mode
/// dontAsk` (denies anything not explicitly listed).
///
/// This is the primary entry point for the loop engine. Non-loop callers
/// (curate, learnings) should use `PermissionMode::text_only()` instead.
pub fn resolve_permission_mode(db_dir: &std::path::Path) -> PermissionMode {
    // 1. LOOP_PERMISSION_MODE takes precedence.
    if let Ok(mode) = std::env::var("LOOP_PERMISSION_MODE") {
        match mode.to_lowercase().as_str() {
            "dangerous" => return PermissionMode::Dangerous,
            "scoped" => {
                // Explicit scoped mode: use CODING_ALLOWED_TOOLS + project config.
                let project_config = super::project_config::read_project_config(db_dir);
                let tools = merge_allowed_tools(&project_config.additional_allowed_tools);
                log_project_config_additions(&project_config.additional_allowed_tools);

                return PermissionMode::Scoped {
                    allowed_tools: Some(tools),
                };
            }
            "auto" => {
                // Explicit auto mode: merge tools just like scoped.
                let project_config = super::project_config::read_project_config(db_dir);
                let tools = merge_allowed_tools(&project_config.additional_allowed_tools);
                log_project_config_additions(&project_config.additional_allowed_tools);

                return PermissionMode::Auto {
                    allowed_tools: Some(tools),
                };
            }
            _ => {
                eprintln!(
                    "\x1b[33m[warn]\x1b[0m Unrecognized LOOP_PERMISSION_MODE='{}', \
                     ignoring; using next-priority resolution. Valid values: 'dangerous', 'scoped', 'auto'.",
                    mode
                );
            }
        }
    }

    // 2. Legacy env var — still supported for backwards compatibility.
    if parse_env_bool("LOOP_ENABLE_AUTO_MODE") == Some(true) {
        let project_config = super::project_config::read_project_config(db_dir);
        let tools = merge_allowed_tools(&project_config.additional_allowed_tools);
        log_project_config_additions(&project_config.additional_allowed_tools);
        return PermissionMode::Auto {
            allowed_tools: Some(tools),
        };
    }

    // 3. Custom allowlist → explicit scoped mode, no project config merge.
    if let Ok(tools) = std::env::var("LOOP_ALLOWED_TOOLS")
        && !tools.is_empty()
    {
        return PermissionMode::Scoped {
            allowed_tools: Some(tools),
        };
    }

    // 4. Project config permissionMode (from .task-mgr/config.json).
    let project_config = super::project_config::read_project_config(db_dir);
    if let Some(ref mode) = project_config.permission_mode {
        match mode.to_lowercase().as_str() {
            "dangerous" => return PermissionMode::Dangerous,
            "scoped" => {
                let tools = merge_allowed_tools(&project_config.additional_allowed_tools);
                log_project_config_additions(&project_config.additional_allowed_tools);
                return PermissionMode::Scoped {
                    allowed_tools: Some(tools),
                };
            }
            "auto" => {
                let tools = merge_allowed_tools(&project_config.additional_allowed_tools);
                log_project_config_additions(&project_config.additional_allowed_tools);
                return PermissionMode::Auto {
                    allowed_tools: Some(tools),
                };
            }
            _ => {
                eprintln!(
                    "\x1b[33m[warn]\x1b[0m Unrecognized permissionMode='{}' in config.json, \
                     ignoring; using next-priority resolution. Valid values: 'dangerous', 'scoped', 'auto'.",
                    mode
                );
            }
        }
    }

    // 5. Default → Dangerous mode (no permission prompts, all tools allowed).
    // The user explicitly opted into this default for the loop engine; project
    // config or LOOP_PERMISSION_MODE can still tighten it back to scoped/auto.
    PermissionMode::Dangerous
}

/// Log project config tool additions to stderr.
fn log_project_config_additions(additional: &[String]) {
    if !additional.is_empty() {
        let names: Vec<&str> = additional
            .iter()
            .filter_map(|t| t.strip_prefix("Bash(").and_then(|s| s.strip_suffix(":*)")))
            .collect();
        eprintln!(
            "\x1b[36m[info]\x1b[0m Project config: +{} tool(s) ({})",
            additional.len(),
            names.join(", ")
        );
    }
}

/// Merge CODING_ALLOWED_TOOLS with additional project-specific tools.
/// Deduplicates entries, preserving insertion order.
fn merge_allowed_tools(additional: &[String]) -> String {
    if additional.is_empty() {
        return CODING_ALLOWED_TOOLS.to_string();
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for tool in CODING_ALLOWED_TOOLS
        .split(',')
        .chain(additional.iter().map(String::as_str))
    {
        let trimmed = tool.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
            result.push(trimmed);
        }
    }
    result.join(",")
}

/// Resolve the permission mode from environment variables only (no project config).
///
/// Resolution order (highest to lowest priority):
/// 1. `LOOP_PERMISSION_MODE=dangerous|scoped|auto` → corresponding variant
/// 2. `LOOP_ENABLE_AUTO_MODE=true` → `Auto` (legacy)
/// 3. `LOOP_ALLOWED_TOOLS=<tools>` → `Scoped { allowed_tools: Some(tools) }`
/// 4. Default → `Dangerous`
///
/// Prefer `resolve_permission_mode(db_dir)` for loop engine use.
/// This function is kept for non-loop callers and tests.
pub fn permission_mode_from_env() -> PermissionMode {
    // 1. Explicit mode takes precedence.
    if let Ok(mode) = std::env::var("LOOP_PERMISSION_MODE") {
        match mode.to_lowercase().as_str() {
            "dangerous" => return PermissionMode::Dangerous,
            "scoped" => {
                let allowed_tools = std::env::var("LOOP_ALLOWED_TOOLS")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| Some(CODING_ALLOWED_TOOLS.to_string()));
                return PermissionMode::Scoped { allowed_tools };
            }
            "auto" => {
                return PermissionMode::Auto {
                    allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
                };
            }
            _ => {
                eprintln!(
                    "\x1b[33m[warn]\x1b[0m Unrecognized LOOP_PERMISSION_MODE='{}', \
                     ignoring; using next-priority resolution. Valid values: 'dangerous', 'scoped', 'auto'.",
                    mode
                );
            }
        }
    }

    // 2. Legacy env var.
    if parse_env_bool("LOOP_ENABLE_AUTO_MODE") == Some(true) {
        return PermissionMode::Auto {
            allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string()),
        };
    }

    // 3. Custom allowlist → explicit scoped mode.
    if let Ok(tools) = std::env::var("LOOP_ALLOWED_TOOLS")
        && !tools.is_empty()
    {
        return PermissionMode::Scoped {
            allowed_tools: Some(tools),
        };
    }

    // 4. Default → Dangerous (mirrors `resolve_permission_mode`'s default).
    PermissionMode::Dangerous
}

/// Types of crashes detected from Claude subprocess exit codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrashType {
    /// Generic runtime error (non-zero exit code, not OOM/segfault/rate-limit)
    RuntimeError,
    /// Process killed by OOM killer or signal (exit code 137)
    OomOrKilled,
    /// Segmentation fault (exit code 139)
    Segfault,
    /// Rate limit error detected in output
    RateLimit,
    /// Claude CLI reported "Prompt is too long" — the running conversation
    /// exceeded the model's context window. Handled by reducing the next
    /// iteration's effort and resetting the task for retry.
    PromptTooLong,
}

/// Outcome of a single loop iteration, determined by analyzing Claude's output.
///
/// Priority order (highest to lowest):
/// Completed > Blocked > Reorder > RateLimit > Crash > NoEligibleTasks > Empty
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IterationOutcome {
    /// All tasks completed successfully
    Completed,
    /// Claude reported a blocker
    Blocked,
    /// Claude requested a different task (contains the requested task ID)
    Reorder(String),
    /// Rate limit detected (don't count against iteration budget)
    RateLimit,
    /// Claude subprocess crashed
    Crash(CrashType),
    /// No eligible tasks to work on (queue empty or all blocked/done)
    NoEligibleTasks,
    /// Claude produced no output (empty response with exit 0)
    Empty,
    /// Prompt critical sections exceed the total character budget
    PromptOverflow,
}

/// A single option in a key decision presented by the agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyDecisionOption {
    /// Short label for the option (e.g., "Use SQLite", "Use PostgreSQL")
    pub label: String,
    /// Longer description explaining the trade-offs of this option
    pub description: String,
}

/// An architectural or strategic decision point flagged by the agent during a loop iteration.
///
/// Key decisions are sideband data — they are NOT variants of `IterationOutcome`.
/// The agent emits `<key-decision>` XML tags to surface important forks that
/// may warrant human review before the loop continues.
///
/// Options may be empty if the tag was malformed; callers should handle gracefully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDecision {
    /// Short title summarising the decision point
    pub title: String,
    /// Longer description providing context and rationale
    pub description: String,
    /// Candidate options for resolving the decision (may be empty)
    pub options: Vec<KeyDecisionOption>,
}

/// Calculate the maximum number of iterations for a given task count.
///
/// Formula: `max(ceil(task_count * 1.75), 5)`
///
/// The 1.75 multiplier provides ~75% headroom for reorders, retries,
/// multi-step tasks, and dynamically spawned tasks (CODE-FIX, WIRE-FIX,
/// REFACTOR, IMPL-FIX tasks generated by review steps). The minimum of 5
/// ensures even small PRDs get enough iterations to complete.
pub fn auto_max_iterations(task_count: usize) -> usize {
    let calculated = ((task_count as f64) * ITERATION_HEADROOM_MULTIPLIER).ceil() as usize;
    calculated.max(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- LoopConfig defaults ---

    #[test]
    fn test_loop_config_default_usage_threshold() {
        let config = LoopConfig::default();
        assert_eq!(
            config.usage_threshold, 92,
            "USAGE_THRESHOLD should default to 92"
        );
    }

    #[test]
    fn test_loop_config_default_max_crashes() {
        let config = LoopConfig::default();
        assert_eq!(config.max_crashes, 3, "MAX_CRASHES should default to 3");
    }

    #[test]
    fn test_loop_config_default_iteration_delay() {
        let config = LoopConfig::default();
        assert_eq!(
            config.iteration_delay_secs, 2,
            "ITERATION_DELAY_SECS should default to 2"
        );
    }

    #[test]
    fn test_loop_config_default_max_iterations_zero() {
        let config = LoopConfig::default();
        assert_eq!(
            config.max_iterations, 0,
            "max_iterations should default to 0 (auto-calculate)"
        );
    }

    #[test]
    fn test_loop_config_default_usage_fallback_wait() {
        let config = LoopConfig::default();
        assert_eq!(config.usage_fallback_wait, 300);
    }

    #[test]
    fn test_loop_config_default_usage_check_enabled() {
        let config = LoopConfig::default();
        assert!(config.usage_check_enabled);
    }

    #[test]
    fn test_loop_config_default_yes_mode_false() {
        let config = LoopConfig::default();
        assert!(!config.yes_mode);
    }

    #[test]
    fn test_loop_config_default_hours_none() {
        let config = LoopConfig::default();
        assert!(config.hours.is_none());
    }

    #[test]
    fn test_loop_config_default_verbose_false() {
        let config = LoopConfig::default();
        assert!(!config.verbose);
    }

    #[test]
    fn test_loop_config_default_use_worktrees_true() {
        let config = LoopConfig::default();
        assert!(config.use_worktrees, "use_worktrees should default to true");
    }

    #[test]
    fn test_loop_config_default_git_scan_depth() {
        let config = LoopConfig::default();
        assert!(
            (1..=20).contains(&config.git_scan_depth),
            "git_scan_depth default {} should be in sane local range 1..=20",
            config.git_scan_depth
        );
    }

    #[test]
    fn test_loop_config_default_external_git_scan_depth() {
        let config = LoopConfig::default();
        assert!(
            (10..=200).contains(&config.external_git_scan_depth),
            "external_git_scan_depth default {} should be in sane external range 10..=200",
            config.external_git_scan_depth
        );
    }

    // --- auto_max_iterations ---

    #[test]
    fn test_auto_max_iterations_minimum_is_5() {
        // Even with 0 tasks, minimum should be 5
        assert_eq!(auto_max_iterations(0), 5);
        assert_eq!(auto_max_iterations(1), 5);
        assert_eq!(auto_max_iterations(2), 5);
    }

    #[test]
    fn test_auto_max_iterations_crosses_minimum_threshold() {
        // First task_count where multiplier produces > 5
        let expected = (5.0 * ITERATION_HEADROOM_MULTIPLIER).ceil() as usize;
        assert!(expected > 5);
        assert_eq!(auto_max_iterations(5), expected);
    }

    #[test]
    fn test_auto_max_iterations_applies_multiplier() {
        for count in [10, 20, 100] {
            let expected = ((count as f64) * ITERATION_HEADROOM_MULTIPLIER).ceil() as usize;
            assert_eq!(auto_max_iterations(count), expected);
        }
    }

    #[test]
    fn test_auto_max_iterations_ceiling_behavior() {
        for count in [7, 9] {
            let expected = ((count as f64) * ITERATION_HEADROOM_MULTIPLIER).ceil() as usize;
            assert_eq!(auto_max_iterations(count), expected);
        }
    }

    // --- IterationOutcome enum variants ---

    #[test]
    fn test_iteration_outcome_completed_variant() {
        let outcome = IterationOutcome::Completed;
        assert_eq!(outcome, IterationOutcome::Completed);
    }

    #[test]
    fn test_iteration_outcome_blocked_variant() {
        let outcome = IterationOutcome::Blocked;
        assert_eq!(outcome, IterationOutcome::Blocked);
    }

    #[test]
    fn test_iteration_outcome_reorder_variant_carries_task_id() {
        let outcome = IterationOutcome::Reorder("LOOP-005".to_string());
        if let IterationOutcome::Reorder(task_id) = &outcome {
            assert_eq!(task_id, "LOOP-005");
        } else {
            panic!("Expected Reorder variant");
        }
    }

    #[test]
    fn test_iteration_outcome_rate_limit_variant() {
        let outcome = IterationOutcome::RateLimit;
        assert_eq!(outcome, IterationOutcome::RateLimit);
    }

    #[test]
    fn test_iteration_outcome_crash_variant_carries_crash_type() {
        let outcome = IterationOutcome::Crash(CrashType::OomOrKilled);
        if let IterationOutcome::Crash(crash_type) = &outcome {
            assert_eq!(*crash_type, CrashType::OomOrKilled);
        } else {
            panic!("Expected Crash variant");
        }
    }

    #[test]
    fn test_iteration_outcome_no_eligible_tasks_variant() {
        let outcome = IterationOutcome::NoEligibleTasks;
        assert_eq!(outcome, IterationOutcome::NoEligibleTasks);
    }

    #[test]
    fn test_no_eligible_tasks_ne_completed() {
        assert_ne!(
            IterationOutcome::NoEligibleTasks,
            IterationOutcome::Completed
        );
    }

    #[test]
    fn test_no_eligible_tasks_ne_empty() {
        assert_ne!(IterationOutcome::NoEligibleTasks, IterationOutcome::Empty);
    }

    #[test]
    fn test_iteration_outcome_empty_variant() {
        let outcome = IterationOutcome::Empty;
        assert_eq!(outcome, IterationOutcome::Empty);
    }

    // --- CrashType enum coverage ---

    #[test]
    fn test_crash_type_runtime_error() {
        let ct = CrashType::RuntimeError;
        assert_eq!(ct, CrashType::RuntimeError);
    }

    #[test]
    fn test_crash_type_oom_or_killed() {
        let ct = CrashType::OomOrKilled;
        assert_eq!(ct, CrashType::OomOrKilled);
    }

    #[test]
    fn test_crash_type_segfault() {
        let ct = CrashType::Segfault;
        assert_eq!(ct, CrashType::Segfault);
    }

    #[test]
    fn test_crash_type_rate_limit() {
        let ct = CrashType::RateLimit;
        assert_eq!(ct, CrashType::RateLimit);
    }

    // --- Equality / inequality ---

    #[test]
    fn test_crash_type_variants_are_distinct() {
        assert_ne!(CrashType::RuntimeError, CrashType::OomOrKilled);
        assert_ne!(CrashType::OomOrKilled, CrashType::Segfault);
        assert_ne!(CrashType::Segfault, CrashType::RateLimit);
        assert_ne!(CrashType::RateLimit, CrashType::RuntimeError);
    }

    #[test]
    fn test_iteration_outcome_variants_are_distinct() {
        assert_ne!(IterationOutcome::Completed, IterationOutcome::Blocked);
        assert_ne!(IterationOutcome::Blocked, IterationOutcome::RateLimit);
        assert_ne!(
            IterationOutcome::RateLimit,
            IterationOutcome::NoEligibleTasks
        );
        assert_ne!(IterationOutcome::NoEligibleTasks, IterationOutcome::Empty);
    }

    #[test]
    fn test_iteration_outcome_reorder_different_ids_not_equal() {
        let a = IterationOutcome::Reorder("TASK-001".to_string());
        let b = IterationOutcome::Reorder("TASK-002".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn test_iteration_outcome_crash_different_types_not_equal() {
        let a = IterationOutcome::Crash(CrashType::OomOrKilled);
        let b = IterationOutcome::Crash(CrashType::Segfault);
        assert_ne!(a, b);
    }

    // --- parse_value helper (thread-safe, no env vars) ---

    #[test]
    fn test_parse_value_valid_usize() {
        assert_eq!(parse_value::<usize>("25"), Some(25));
    }

    #[test]
    fn test_parse_value_valid_u8() {
        assert_eq!(parse_value::<u8>("85"), Some(85));
    }

    #[test]
    fn test_parse_value_valid_u64() {
        assert_eq!(parse_value::<u64>("600"), Some(600));
    }

    #[test]
    fn test_parse_value_invalid_returns_none() {
        assert_eq!(parse_value::<usize>("not_a_number"), None);
    }

    #[test]
    fn test_parse_value_overflow_u8_returns_none() {
        assert_eq!(parse_value::<u8>("256"), None);
    }

    #[test]
    fn test_parse_value_negative_u64_returns_none() {
        assert_eq!(parse_value::<u64>("-1"), None);
    }

    #[test]
    fn test_parse_value_empty_string_returns_none() {
        assert_eq!(parse_value::<usize>(""), None);
    }

    #[test]
    fn test_parse_value_whitespace_returns_none() {
        assert_eq!(parse_value::<u32>("  "), None);
    }

    // --- parse_bool_value (thread-safe, no env vars) ---

    #[test]
    fn test_parse_bool_value_true_variants() {
        for val in &["true", "1", "yes", "TRUE", "Yes", "YES"] {
            assert_eq!(
                parse_bool_value(val),
                Some(true),
                "Expected true for '{val}'"
            );
        }
    }

    #[test]
    fn test_parse_bool_value_false_variants() {
        for val in &["false", "0", "no", "FALSE", "No", "NO"] {
            assert_eq!(
                parse_bool_value(val),
                Some(false),
                "Expected false for '{val}'"
            );
        }
    }

    #[test]
    fn test_parse_bool_value_unrecognized_returns_none() {
        assert_eq!(parse_bool_value("maybe"), None);
        assert_eq!(parse_bool_value("dunno"), None);
        assert_eq!(parse_bool_value(""), None);
        assert_eq!(parse_bool_value("2"), None);
    }

    // --- from_env() ---
    // These tests exercise the env-reading integration path.
    // They use unique var names to minimize parallel interference.

    #[test]
    fn test_from_env_cli_only_fields_always_default() {
        // yes_mode, hours, verbose, use_worktrees are CLI-only, never read from env
        let config = LoopConfig::from_env();
        assert!(!config.yes_mode);
        assert!(config.hours.is_none());
        assert!(!config.verbose);
        assert!(config.use_worktrees); // defaults to true
    }

    #[test]
    fn test_loop_config_default_parallel_slots() {
        let config = LoopConfig::default();
        assert_eq!(
            config.parallel_slots, 2,
            "parallel_slots should default to 2"
        );
    }

    #[test]
    fn test_parse_env_missing_var_returns_none() {
        // Use a unique name that no test sets
        let result: Option<u32> = parse_env("TASKMGR_NONEXISTENT_VAR_49817");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_env_bool_missing_var_returns_none() {
        assert!(parse_env_bool("TASKMGR_NONEXISTENT_BOOL_49817").is_none());
    }

    // --- LOOP_PARALLEL env var parsing ---
    // These tests mutate environment variables and must be serialised.

    static PARALLEL_ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_loop_parallel_env_valid_values() {
        for n in 1usize..=3 {
            let _guard = PARALLEL_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
            unsafe { std::env::set_var("LOOP_PARALLEL", n.to_string()) };
            let config = LoopConfig::from_env();
            unsafe { std::env::remove_var("LOOP_PARALLEL") };
            assert_eq!(
                config.parallel_slots, n,
                "LOOP_PARALLEL={n} should set parallel_slots={n}"
            );
        }
    }

    #[test]
    fn test_loop_parallel_env_zero_rejected() {
        let _guard = PARALLEL_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PARALLEL", "0") };
        let config = LoopConfig::from_env();
        unsafe { std::env::remove_var("LOOP_PARALLEL") };
        assert_eq!(
            config.parallel_slots, 2,
            "LOOP_PARALLEL=0 should fall back to default 2"
        );
    }

    #[test]
    fn test_loop_parallel_env_above_max_rejected() {
        let _guard = PARALLEL_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PARALLEL", "4") };
        let config = LoopConfig::from_env();
        unsafe { std::env::remove_var("LOOP_PARALLEL") };
        assert_eq!(
            config.parallel_slots, 2,
            "LOOP_PARALLEL=4 should fall back to default 2"
        );
    }

    #[test]
    fn test_loop_parallel_env_invalid_string_rejected() {
        let _guard = PARALLEL_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PARALLEL", "abc") };
        let config = LoopConfig::from_env();
        unsafe { std::env::remove_var("LOOP_PARALLEL") };
        assert_eq!(
            config.parallel_slots, 2,
            "LOOP_PARALLEL=abc should fall back to default 2"
        );
    }

    // --- PermissionMode enum ---

    #[test]
    fn test_permission_mode_dangerous_variant() {
        let mode = PermissionMode::Dangerous;
        assert_eq!(mode, PermissionMode::Dangerous);
    }

    #[test]
    fn test_permission_mode_auto_variant() {
        let mode = PermissionMode::Auto {
            allowed_tools: None,
        };
        assert!(matches!(mode, PermissionMode::Auto { .. }));
    }

    #[test]
    fn test_permission_mode_scoped_none() {
        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };
        assert_eq!(
            mode,
            PermissionMode::Scoped {
                allowed_tools: None
            }
        );
    }

    #[test]
    fn test_permission_mode_scoped_some() {
        let mode = PermissionMode::Scoped {
            allowed_tools: Some("Read,Edit".to_string()),
        };
        if let PermissionMode::Scoped { allowed_tools } = &mode {
            assert_eq!(allowed_tools.as_deref(), Some("Read,Edit"));
        } else {
            panic!("Expected Scoped variant");
        }
    }

    #[test]
    fn test_permission_mode_variants_distinct() {
        assert_ne!(
            PermissionMode::Dangerous,
            PermissionMode::Auto {
                allowed_tools: None
            }
        );
        assert_ne!(
            PermissionMode::Dangerous,
            PermissionMode::Scoped {
                allowed_tools: None
            }
        );
        assert_ne!(
            PermissionMode::Auto {
                allowed_tools: None
            },
            PermissionMode::Scoped {
                allowed_tools: None
            }
        );
    }

    #[test]
    fn test_permission_mode_clone() {
        let mode = PermissionMode::Scoped {
            allowed_tools: Some("Read".to_string()),
        };
        assert_eq!(mode.clone(), mode);
    }

    // --- PermissionMode Display ---

    #[test]
    fn test_permission_mode_display_dangerous() {
        assert_eq!(
            PermissionMode::Dangerous.to_string(),
            "Dangerous (no tool restrictions)"
        );
    }

    #[test]
    fn test_permission_mode_display_auto() {
        assert_eq!(
            PermissionMode::Auto {
                allowed_tools: None
            }
            .to_string(),
            "Auto"
        );
    }

    #[test]
    fn test_permission_mode_display_auto_with_tools() {
        let mode = PermissionMode::Auto {
            allowed_tools: Some("Read,Edit,Bash(cargo:*)".to_string()),
        };
        assert_eq!(mode.to_string(), "Auto (3 tools)");
    }

    #[test]
    fn test_permission_mode_display_scoped_none() {
        let mode = PermissionMode::Scoped {
            allowed_tools: None,
        };
        assert_eq!(mode.to_string(), "Scoped (text-only, no tools)");
    }

    #[test]
    fn test_permission_mode_display_scoped_with_tools() {
        let mode = PermissionMode::Scoped {
            allowed_tools: Some("Read,Edit,Bash(cargo:*)".to_string()),
        };
        assert_eq!(mode.to_string(), "Scoped (3 tools)");
    }

    // --- CODING_ALLOWED_TOOLS / TASKS_JSON_DISALLOWED_TOOLS constants ---

    #[test]
    fn test_coding_allowed_tools_permits_task_mgr_bash() {
        assert!(
            CODING_ALLOWED_TOOLS.contains("Bash(task-mgr:*)"),
            "CODING_ALLOWED_TOOLS must contain 'Bash(task-mgr:*)' so the loop agent can use task-mgr CLI"
        );
    }

    #[test]
    fn test_tasks_json_disallowed_tools_is_path_scoped() {
        // Known-bad guard: a blanket "Edit" deny would block legitimate src/**/*.rs edits.
        // The constant must reference path patterns, not deny the tool wholesale.
        assert!(
            !TASKS_JSON_DISALLOWED_TOOLS.starts_with("Edit,")
                && TASKS_JSON_DISALLOWED_TOOLS != "Edit"
                && !TASKS_JSON_DISALLOWED_TOOLS.starts_with("Write,")
                && TASKS_JSON_DISALLOWED_TOOLS != "Write",
            "TASKS_JSON_DISALLOWED_TOOLS must use path-scoped patterns, not blanket Edit/Write denials"
        );
        // Must target the tasks JSON path specifically.
        assert!(
            TASKS_JSON_DISALLOWED_TOOLS.contains(".task-mgr/tasks/"),
            "TASKS_JSON_DISALLOWED_TOOLS must reference .task-mgr/tasks/ path"
        );
        assert!(
            TASKS_JSON_DISALLOWED_TOOLS.contains(".json"),
            "TASKS_JSON_DISALLOWED_TOOLS must target .json files"
        );
    }

    #[test]
    fn test_coding_allowed_tools_contains_required_tools() {
        let tools = CODING_ALLOWED_TOOLS;
        for required in &[
            "Read",
            "Edit",
            "Write",
            "WebFetch",
            "WebSearch",
            "NotebookEdit",
            "Agent",
            "LSP",
            "Bash(cargo:*)",
            "Bash(git:*)",
            "Bash(task-mgr:*)",
            "Bash(mkdir:*)",
            "Bash(ls:*)",
            "Bash(wc:*)",
            "Bash(head:*)",
            "Bash(tail:*)",
            "Bash(cat:*)",
            "Bash(find:*)",
            "Bash(rg:*)",
            "Bash(sed:*)",
            "Bash(cd:*)",
            "Bash(ruff:*)",
            "Bash(mypy:*)",
            "Bash(uv:*)",
            "Bash(pytest:*)",
            "Bash(python:*)",
            "Bash(pip:*)",
            "Bash(npm:*)",
            "Bash(npx:*)",
            "Bash(node:*)",
            "Bash(bun:*)",
            "Bash(pnpm:*)",
            "Bash(yarn:*)",
            "Bash(make:*)",
            "Bash(grep:*)",
            "Bash(awk:*)",
            "Bash(sort:*)",
            "Bash(uniq:*)",
            "Bash(tr:*)",
            "Bash(cut:*)",
            "Bash(diff:*)",
            "Bash(touch:*)",
            "Bash(cp:*)",
            "Bash(mv:*)",
            "Bash(rm:*)",
            "Bash(chmod:*)",
            "Bash(echo:*)",
            "Bash(printf:*)",
            "Bash(tee:*)",
            "Bash(xargs:*)",
            "Bash(jq:*)",
            "Bash(yq:*)",
            "Bash(tree:*)",
            "Bash(which:*)",
            "Bash(command:*)",
            "Bash(pwd:*)",
            "Bash(realpath:*)",
            "Bash(dirname:*)",
            "Bash(basename:*)",
            "Bash(date:*)",
            "Bash(stat:*)",
            "Bash(env:*)",
            "Bash(rustup:*)",
            "Bash(mix:*)",
            "Bash(elixir:*)",
            "Bash(iex:*)",
            "Bash(hex:*)",
            "Bash(rebar3:*)",
            "Bash(shellcheck:*)",
            "Bash(shfmt:*)",
            "Bash(strings:*)",
            "Bash(file:*)",
            "Glob",
            "Grep",
        ] {
            assert!(
                tools.contains(required),
                "CODING_ALLOWED_TOOLS missing '{required}'"
            );
        }
    }

    #[test]
    fn test_coding_allowed_tools_excludes_per_project_tools() {
        let tools = CODING_ALLOWED_TOOLS;
        for excluded in &[
            "Bash(curl:*)",
            "Bash(wget:*)",
            "Bash(docker:*)",
            "Bash(docker-compose:*)",
            "Bash(source:*)",
            "Bash(./scripts/*:*)",
            "Bash(test:*)",
            "Bash([:*)",
        ] {
            assert!(
                !tools.contains(excluded),
                "CODING_ALLOWED_TOOLS should NOT contain '{excluded}' (moved to per-project config)"
            );
        }
    }

    // --- permission_mode_from_env() ---
    // These tests mutate environment variables and must be serialised.

    use std::sync::Mutex;

    static PERM_ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_permission_mode_from_env_default_is_dangerous() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let mode = permission_mode_from_env();
        assert_eq!(
            mode,
            PermissionMode::Dangerous,
            "Default should be Dangerous (no permission prompts), got {:?}",
            mode
        );
    }

    #[test]
    fn test_permission_mode_from_env_dangerous_mode() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "dangerous") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        assert_eq!(mode, PermissionMode::Dangerous);
    }

    #[test]
    fn test_permission_mode_from_env_auto_mode() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::set_var("LOOP_ENABLE_AUTO_MODE", "true") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        assert!(
            matches!(
                mode,
                PermissionMode::Auto {
                    allowed_tools: Some(_)
                }
            ),
            "Legacy auto should be Auto with tools, got {:?}",
            mode
        );
    }

    #[test]
    fn test_permission_mode_from_env_custom_allowed_tools() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::set_var("LOOP_ALLOWED_TOOLS", "Read,Bash") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };
        assert_eq!(
            mode,
            PermissionMode::Scoped {
                allowed_tools: Some("Read,Bash".to_string())
            }
        );
    }

    /// Known-bad guard: if auto is checked before dangerous, setting both would
    /// return Auto instead of Dangerous. Verify Dangerous wins.
    #[test]
    fn test_permission_mode_from_env_dangerous_beats_auto() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "dangerous") };
        unsafe { std::env::set_var("LOOP_ENABLE_AUTO_MODE", "true") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        assert_eq!(mode, PermissionMode::Dangerous);
    }

    #[test]
    fn test_permission_mode_from_env_unknown_mode_falls_through_to_dangerous() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "unknown") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        assert_eq!(
            mode,
            PermissionMode::Dangerous,
            "Unknown mode should warn and fall through to the default (Dangerous), got {:?}",
            mode
        );
    }

    #[test]
    fn test_permission_mode_from_env_empty_allowed_tools_falls_back_to_dangerous() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::set_var("LOOP_ALLOWED_TOOLS", "") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };
        assert_eq!(
            mode,
            PermissionMode::Dangerous,
            "Empty LOOP_ALLOWED_TOOLS should fall through to the default (Dangerous), got {:?}",
            mode
        );
    }

    #[test]
    fn test_permission_mode_from_env_scoped_explicit() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "scoped") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let mode = permission_mode_from_env();
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        assert_eq!(
            mode,
            PermissionMode::Scoped {
                allowed_tools: Some(CODING_ALLOWED_TOOLS.to_string())
            }
        );
    }

    // Negative: PermissionMode is NOT a field on LoopConfig — verified at compile time.
    // The LoopConfig struct has no permission_mode field; this test exists to document
    // the negative acceptance criterion.
    #[test]
    fn test_loop_config_has_no_permission_mode_field() {
        let config = LoopConfig::default();
        // If LoopConfig had a permission_mode field, this destructuring would need it.
        let LoopConfig {
            max_iterations: _,
            usage_threshold: _,
            max_crashes: _,
            iteration_delay_secs: _,
            usage_fallback_wait: _,
            usage_check_enabled: _,
            yes_mode: _,
            hours: _,
            verbose: _,
            use_worktrees: _,
            git_scan_depth: _,
            external_git_scan_depth: _,
            cleanup_worktree: _,
            parallel_slots: _,
        } = config;
        // Exhaustive destructure compiles only if LoopConfig has exactly these fields.
    }

    // --- Clone behavior ---

    #[test]
    fn test_loop_config_clone() {
        let config = LoopConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.usage_threshold, config.usage_threshold);
        assert_eq!(cloned.max_crashes, config.max_crashes);
        assert_eq!(cloned.iteration_delay_secs, config.iteration_delay_secs);
    }

    #[test]
    fn test_crash_type_clone() {
        let ct = CrashType::Segfault;
        let cloned = ct.clone();
        assert_eq!(ct, cloned);
    }

    #[test]
    fn test_iteration_outcome_clone() {
        let outcome = IterationOutcome::Reorder("FEAT-001".to_string());
        let cloned = outcome.clone();
        assert_eq!(outcome, cloned);
    }

    // --- KeyDecision / KeyDecisionOption structs ---

    #[test]
    fn test_key_decision_option_fields() {
        let opt = KeyDecisionOption {
            label: "Use SQLite".to_string(),
            description: "Simpler, no server needed".to_string(),
        };
        assert_eq!(opt.label, "Use SQLite");
        assert_eq!(opt.description, "Simpler, no server needed");
    }

    #[test]
    fn test_key_decision_option_clone_eq() {
        let opt = KeyDecisionOption {
            label: "A".to_string(),
            description: "B".to_string(),
        };
        assert_eq!(opt.clone(), opt);
    }

    #[test]
    fn test_key_decision_fields() {
        let kd = KeyDecision {
            title: "Storage backend".to_string(),
            description: "Choose between SQLite and PostgreSQL".to_string(),
            options: vec![
                KeyDecisionOption {
                    label: "SQLite".to_string(),
                    description: "Embedded".to_string(),
                },
                KeyDecisionOption {
                    label: "PostgreSQL".to_string(),
                    description: "Server-based".to_string(),
                },
            ],
        };
        assert_eq!(kd.title, "Storage backend");
        assert_eq!(kd.options.len(), 2);
    }

    #[test]
    fn test_key_decision_empty_options_allowed() {
        let kd = KeyDecision {
            title: "Malformed tag".to_string(),
            description: "No options provided".to_string(),
            options: vec![],
        };
        assert!(kd.options.is_empty());
    }

    #[test]
    fn test_key_decision_clone_eq() {
        let kd = KeyDecision {
            title: "T".to_string(),
            description: "D".to_string(),
            options: vec![],
        };
        assert_eq!(kd.clone(), kd);
    }

    #[test]
    fn test_key_decision_ne_different_title() {
        let a = KeyDecision {
            title: "A".to_string(),
            description: "D".to_string(),
            options: vec![],
        };
        let b = KeyDecision {
            title: "B".to_string(),
            description: "D".to_string(),
            options: vec![],
        };
        assert_ne!(a, b);
    }

    #[test]
    fn test_key_decision_is_not_iteration_outcome_variant() {
        // Compile-time proof: KeyDecision is a struct, not an IterationOutcome variant.
        // If this compiles, the negative acceptance criterion is satisfied.
        let _kd: KeyDecision = KeyDecision {
            title: String::new(),
            description: String::new(),
            options: vec![],
        };
        // IterationOutcome has no KeyDecision variant — verified by exhaustive match below.
        let outcome = IterationOutcome::Completed;
        let _covered = matches!(
            outcome,
            IterationOutcome::Completed
                | IterationOutcome::Blocked
                | IterationOutcome::Reorder(_)
                | IterationOutcome::RateLimit
                | IterationOutcome::Crash(_)
                | IterationOutcome::NoEligibleTasks
                | IterationOutcome::Empty
                | IterationOutcome::PromptOverflow
        );
    }

    // --- merge_allowed_tools ---

    #[test]
    fn test_merge_allowed_tools_empty_returns_default() {
        let result = merge_allowed_tools(&[]);
        assert_eq!(result, CODING_ALLOWED_TOOLS);
    }

    #[test]
    fn test_merge_allowed_tools_appends() {
        let additional = vec!["Bash(docker:*)".to_string(), "Bash(curl:*)".to_string()];
        let result = merge_allowed_tools(&additional);
        assert!(result.contains("Bash(docker:*)"));
        assert!(result.contains("Bash(curl:*)"));
        // Core tools still present
        assert!(result.starts_with("Read,"));
        assert!(result.contains("Bash(cargo:*)"));
    }

    #[test]
    fn test_merge_allowed_tools_deduplicates() {
        // Bash(git:*) is already in CODING_ALLOWED_TOOLS
        let additional = vec!["Bash(git:*)".to_string(), "Bash(docker:*)".to_string()];
        let result = merge_allowed_tools(&additional);
        // Count occurrences of Bash(git:*)
        let count = result.matches("Bash(git:*)").count();
        assert_eq!(count, 1, "Bash(git:*) should appear exactly once");
        // New tool still added
        assert!(result.contains("Bash(docker:*)"));
    }

    // --- resolve_permission_mode ---

    #[test]
    fn test_resolve_permission_mode_default_is_dangerous() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        // Use a temp dir with no config.json
        let dir = tempfile::tempdir().unwrap();
        let mode = resolve_permission_mode(dir.path());
        assert_eq!(
            mode,
            PermissionMode::Dangerous,
            "Default should be Dangerous (no permission prompts), got {:?}",
            mode
        );
    }

    #[test]
    fn test_resolve_permission_mode_env_override_skips_project_config() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::set_var("LOOP_ALLOWED_TOOLS", "Read,Bash") };

        // Create a dir WITH config.json — it should be ignored
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"additionalAllowedTools": ["Bash(docker:*)"]}"#,
        )
        .unwrap();

        let mode = resolve_permission_mode(dir.path());
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };
        assert_eq!(
            mode,
            PermissionMode::Scoped {
                allowed_tools: Some("Read,Bash".to_string())
            }
        );
    }

    #[test]
    fn test_resolve_permission_mode_dangerous_skips_all() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "dangerous") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let dir = tempfile::tempdir().unwrap();
        let mode = resolve_permission_mode(dir.path());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        assert_eq!(mode, PermissionMode::Dangerous);
    }

    #[test]
    fn test_resolve_permission_mode_scoped_merges_project_config() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "scoped") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"additionalAllowedTools": ["Bash(docker:*)", "Bash(curl:*)"]}"#,
        )
        .unwrap();

        let mode = resolve_permission_mode(dir.path());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        if let PermissionMode::Scoped {
            allowed_tools: Some(tools),
        } = &mode
        {
            assert!(tools.contains("Bash(docker:*)"));
            assert!(tools.contains("Bash(curl:*)"));
            assert!(tools.contains("Bash(cargo:*)")); // core still present
        } else {
            panic!("Expected Scoped with tools, got {:?}", mode);
        }
    }

    #[test]
    fn test_resolve_permission_mode_default_ignores_additional_allowed_tools() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        // additionalAllowedTools is meaningful only when permissionMode is
        // explicitly scoped/auto. With the Dangerous default, it's ignored
        // because Dangerous doesn't use an allowlist at all.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"additionalAllowedTools": ["Bash(docker:*)"]}"#,
        )
        .unwrap();

        let mode = resolve_permission_mode(dir.path());
        assert_eq!(
            mode,
            PermissionMode::Dangerous,
            "Default should be Dangerous regardless of additionalAllowedTools, got {:?}",
            mode
        );
    }

    #[test]
    fn test_resolve_permission_mode_project_config_dangerous() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"permissionMode": "dangerous"}"#,
        )
        .unwrap();

        let mode = resolve_permission_mode(dir.path());
        assert_eq!(mode, PermissionMode::Dangerous);
    }

    #[test]
    fn test_resolve_permission_mode_env_overrides_project_config() {
        let _guard = PERM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("LOOP_PERMISSION_MODE", "scoped") };
        unsafe { std::env::remove_var("LOOP_ENABLE_AUTO_MODE") };
        unsafe { std::env::remove_var("LOOP_ALLOWED_TOOLS") };

        let dir = tempfile::tempdir().unwrap();
        // Project config says dangerous, but env says scoped — env wins
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"permissionMode": "dangerous"}"#,
        )
        .unwrap();

        let mode = resolve_permission_mode(dir.path());
        unsafe { std::env::remove_var("LOOP_PERMISSION_MODE") };
        assert!(
            matches!(mode, PermissionMode::Scoped { .. }),
            "Env var should override project config, got {:?}",
            mode
        );
    }
}
