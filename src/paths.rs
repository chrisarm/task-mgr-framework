//! XDG-style path resolution for per-user config and cache files.
//!
//! Linux + macOS only (CI matrix). Respects `XDG_CONFIG_HOME` /
//! `XDG_CACHE_HOME` when set, else falls back to `$HOME/.config` and
//! `$HOME/.cache` respectively.

use std::path::PathBuf;

/// Directory for per-user config files (e.g.
/// `$XDG_CONFIG_HOME/task-mgr/config.json`).
///
/// Returns `None` if neither `XDG_CONFIG_HOME` nor `HOME` is set.
pub fn user_config_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Directory for per-user cache files (e.g.
/// `$XDG_CACHE_HOME/task-mgr/models-cache.json`).
///
/// Returns `None` if neither `XDG_CACHE_HOME` nor `HOME` is set.
pub fn user_cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

fn xdg_dir(xdg_var: &str, home_subdir: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(xdg_var) {
        let path = PathBuf::from(v);
        if path.is_absolute() {
            return Some(path.join("task-mgr"));
        }
        // XDG spec: relative paths are invalid; fall through to HOME.
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_subdir).join("task-mgr"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard that restores an env var on drop. Tests that mutate process env
    /// must run under a mutex because Rust runs tests in parallel.
    struct EnvGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, prior }
        }
        fn unset(key: &'static str) -> Self {
            let prior = std::env::var_os(key);
            unsafe { std::env::remove_var(key) };
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    use std::sync::Mutex;
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn xdg_config_home_is_respected_when_absolute() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::set("XDG_CONFIG_HOME", "/custom/xdg/config");
        assert_eq!(
            user_config_dir(),
            Some(PathBuf::from("/custom/xdg/config/task-mgr"))
        );
    }

    #[test]
    fn relative_xdg_config_home_falls_back_to_home() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _gx = EnvGuard::set("XDG_CONFIG_HOME", "relative/path");
        let _gh = EnvGuard::set("HOME", "/home/test");
        assert_eq!(
            user_config_dir(),
            Some(PathBuf::from("/home/test/.config/task-mgr"))
        );
    }

    #[test]
    fn config_falls_back_to_home_when_xdg_unset() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _gx = EnvGuard::unset("XDG_CONFIG_HOME");
        let _gh = EnvGuard::set("HOME", "/home/test");
        assert_eq!(
            user_config_dir(),
            Some(PathBuf::from("/home/test/.config/task-mgr"))
        );
    }

    #[test]
    fn cache_falls_back_to_home_when_xdg_unset() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _gx = EnvGuard::unset("XDG_CACHE_HOME");
        let _gh = EnvGuard::set("HOME", "/home/test");
        assert_eq!(
            user_cache_dir(),
            Some(PathBuf::from("/home/test/.cache/task-mgr"))
        );
    }

    #[test]
    fn returns_none_when_no_env_available() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _gx = EnvGuard::unset("XDG_CONFIG_HOME");
        let _gh = EnvGuard::unset("HOME");
        assert_eq!(user_config_dir(), None);
    }
}
