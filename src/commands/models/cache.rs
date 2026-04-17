//! 24h cache for `/v1/models` responses at `$XDG_CACHE_HOME/task-mgr/models-cache.json`.
//!
//! Stale-cache-as-miss semantics: when the cache is older than TTL, we treat
//! it as absent. No "stale-on-error" — if the live fetch fails, callers fall
//! back to the hardcoded constants in `loop_engine::model`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::api::RemoteModel;
use crate::paths::user_cache_dir;

const CACHE_FILENAME: &str = "models-cache.json";
const CURRENT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// On-disk cache layout. Versioned so future cache invalidations can happen
/// by bumping `schema_version` — older caches are then treated as misses.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    schema_version: u32,
    fetched_at: DateTime<Utc>,
    ttl_seconds: u64,
    models: Vec<RemoteModel>,
}

/// Return the resolved cache-file path, or `None` if neither `XDG_CACHE_HOME`
/// nor `HOME` is set.
pub fn cache_path() -> Option<PathBuf> {
    user_cache_dir().map(|d| d.join(CACHE_FILENAME))
}

/// Read the cache if present AND fresh. Stale / missing / corrupt / wrong
/// schema version → `None` (treated as miss). Never surfaces an error — the
/// caller is expected to refetch or fall back.
pub fn read_fresh() -> Option<Vec<RemoteModel>> {
    let path = cache_path()?;
    read_fresh_at(&path, DEFAULT_TTL, Utc::now())
}

/// Testable variant: explicit path, TTL, and "now" clock.
pub fn read_fresh_at(path: &Path, ttl: Duration, now: DateTime<Utc>) -> Option<Vec<RemoteModel>> {
    let contents = std::fs::read_to_string(path).ok()?;
    let cache: CacheFile = serde_json::from_str(&contents).ok()?;
    if cache.schema_version != CURRENT_SCHEMA_VERSION {
        return None;
    }
    let age = now.signed_duration_since(cache.fetched_at);
    // Reject negative ages (clock skew / fetched in the future) — treat as stale.
    let age_secs = age.num_seconds();
    if age_secs < 0 || age_secs as u64 >= ttl.as_secs() {
        return None;
    }
    Some(cache.models)
}

/// Write (or replace) the cache. Creates parent dir and writes atomically
/// via a same-directory tempfile + rename. Failures print a one-line stderr
/// hint but don't propagate — callers shouldn't fail a read-only flow over a
/// cache-write error.
pub fn write(models: &[RemoteModel]) {
    let Some(path) = cache_path() else { return };
    if let Err(e) = write_at(&path, models, Utc::now()) {
        eprintln!(
            "\x1b[33m[warn]\x1b[0m models cache write failed ({}): {e}",
            path.display()
        );
    }
}

/// Testable variant: explicit path and "now" clock.
pub fn write_at(path: &Path, models: &[RemoteModel], now: DateTime<Utc>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cache = CacheFile {
        schema_version: CURRENT_SCHEMA_VERSION,
        fetched_at: now,
        ttl_seconds: DEFAULT_TTL.as_secs(),
        models: models.to_vec(),
    };
    let contents = serde_json::to_string(&cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".models-cache-")
        .suffix(".json")
        .tempfile_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Invalidate the cache by deleting the file. Missing file is not an error.
pub fn invalidate() {
    if let Some(path) = cache_path() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::model::OPUS_MODEL;

    fn sample() -> Vec<RemoteModel> {
        vec![RemoteModel {
            id: OPUS_MODEL.to_string(),
            display_name: Some("Claude Opus".to_string()),
            created_at: Some(Utc::now()),
            kind: Some("model".to_string()),
        }]
    }

    #[test]
    fn round_trip_within_ttl_returns_cached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let now = Utc::now();
        write_at(&path, &sample(), now).unwrap();
        let read = read_fresh_at(&path, Duration::from_secs(60), now).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id, OPUS_MODEL);
    }

    #[test]
    fn stale_cache_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let fetched = Utc::now();
        write_at(&path, &sample(), fetched).unwrap();
        let future = fetched + chrono::Duration::seconds(120);
        let result = read_fresh_at(&path, Duration::from_secs(60), future);
        assert!(result.is_none(), "cache older than TTL should be a miss");
    }

    #[test]
    fn missing_file_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let result = read_fresh_at(&path, DEFAULT_TTL, Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn corrupt_json_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        std::fs::write(&path, "not json").unwrap();
        let result = read_fresh_at(&path, DEFAULT_TTL, Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn wrong_schema_version_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        std::fs::write(
            &path,
            r#"{"schema_version": 999, "fetched_at": "2026-01-01T00:00:00Z", "ttl_seconds": 86400, "models": []}"#,
        )
        .unwrap();
        let result = read_fresh_at(&path, DEFAULT_TTL, Utc::now());
        assert!(result.is_none());
    }

    #[test]
    fn negative_age_is_miss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let future = Utc::now() + chrono::Duration::seconds(3600);
        write_at(&path, &sample(), future).unwrap();
        let now = Utc::now();
        let result = read_fresh_at(&path, DEFAULT_TTL, now);
        assert!(result.is_none(), "cache fetched in the future is a miss");
    }

    #[test]
    fn write_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/subdir/cache.json");
        write_at(&path, &sample(), Utc::now()).unwrap();
        assert!(path.is_file());
    }
}
