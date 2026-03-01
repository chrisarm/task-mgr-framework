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
fn parse_bool_value(value: &str) -> Option<bool> {
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
}

/// Outcome of a single loop iteration, determined by analyzing Claude's output.
///
/// Priority order (highest to lowest):
/// Completed > Blocked > Reorder > RateLimit > Crash > Stale > Empty
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
    /// No progress detected (same DB state before and after)
    Stale,
    /// Claude produced no output (empty response with exit 0)
    Empty,
    /// Prompt critical sections exceed the total character budget
    PromptOverflow,
}

/// Calculate the maximum number of iterations for a given task count.
///
/// Formula: `max(ceil(task_count * 1.12), 5)`
///
/// The 1.12 multiplier provides ~12% headroom for reorders, retries, and
/// multi-step tasks. The minimum of 5 ensures even small PRDs get enough
/// iterations to complete.
pub fn auto_max_iterations(task_count: usize) -> usize {
    let calculated = ((task_count as f64) * 1.12).ceil() as usize;
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
        assert_eq!(auto_max_iterations(3), 5);
        assert_eq!(auto_max_iterations(4), 5);
    }

    #[test]
    fn test_auto_max_iterations_crosses_minimum_threshold() {
        // 5 * 1.12 = 5.6 -> ceil = 6, which is > 5
        assert_eq!(auto_max_iterations(5), 6);
    }

    #[test]
    fn test_auto_max_iterations_applies_multiplier() {
        // 10 * 1.12 = 11.2 -> ceil = 12
        assert_eq!(auto_max_iterations(10), 12);
        // 20 * 1.12 = 22.4 -> ceil = 23
        assert_eq!(auto_max_iterations(20), 23);
        // 100 * 1.12 = 112.0 (but f64 gives 112.00000000000001) -> ceil = 113
        assert_eq!(auto_max_iterations(100), 113);
    }

    #[test]
    fn test_auto_max_iterations_ceiling_behavior() {
        // 7 * 1.12 = 7.84 -> ceil = 8
        assert_eq!(auto_max_iterations(7), 8);
        // 9 * 1.12 = 10.08 -> ceil = 11
        assert_eq!(auto_max_iterations(9), 11);
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
    fn test_iteration_outcome_stale_variant() {
        let outcome = IterationOutcome::Stale;
        assert_eq!(outcome, IterationOutcome::Stale);
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
        assert_ne!(IterationOutcome::RateLimit, IterationOutcome::Stale);
        assert_ne!(IterationOutcome::Stale, IterationOutcome::Empty);
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
    fn test_parse_env_missing_var_returns_none() {
        // Use a unique name that no test sets
        let result: Option<u32> = parse_env("TASKMGR_NONEXISTENT_VAR_49817");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_env_bool_missing_var_returns_none() {
        assert!(parse_env_bool("TASKMGR_NONEXISTENT_BOOL_49817").is_none());
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
}
