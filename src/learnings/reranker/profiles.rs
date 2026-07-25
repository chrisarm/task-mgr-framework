//! Built-in reranker model profiles for recall cross-encoder stage.
//!
//! Profiles select the llama-box model name and document/query char caps sized
//! for each model's training context. Operators set `rerankerProfile` in
//! `.task-mgr/config.json` (with `rerankerUrl`), or pass raw `rerankerModel`.

/// Default reranker profile id (Jina v2 multilingual).
pub const DEFAULT_RERANKER_PROFILE_ID: &str = "jina-v2";

/// Default model string sent to llama-box for the Jina profile (matches baked GGUF basename).
pub const DEFAULT_RERANKER_MODEL: &str = "jina-reranker-v2-base-multilingual";

/// Jina-oriented document char cap (1024-token training ctx).
pub const JINA_MAX_DOC_CHARS: usize = 1024;

/// Jina-oriented query char cap.
pub const JINA_MAX_QUERY_CHARS: usize = 256;

/// One catalog entry for a cross-encoder served by llama-box `/v1/rerank`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RerankerProfile {
    /// Stable id used in config (`rerankerProfile`).
    pub id: &'static str,
    /// `model` field in the `/v1/rerank` JSON body (must match server advertising).
    pub model: &'static str,
    /// Max Unicode chars per document sent to the reranker.
    pub max_doc_chars: usize,
    /// Max Unicode chars in the query.
    pub max_query_chars: usize,
    /// Optional docker/bake hints for operators (HF repo + file).
    pub hf_repo: Option<&'static str>,
    pub hf_file: Option<&'static str>,
    /// Basename of the GGUF as loaded by llama-box (`--model /models/...`).
    pub container_model_path: Option<&'static str>,
    pub notes: &'static str,
}

/// Default extra percent beyond `limit` when fetching the rerank slate.
///
/// 200 → fetch `ceil(limit * 3.0)` (same effective size as the old integer
/// multiplier default of `3`). Example: limit 10 → slate 30.
pub const DEFAULT_RERANKER_OVER_FETCH_PERCENT: u32 = 200;

/// Maximum allowed `rerankerOverFetchPercent` (extra % beyond limit).
///
/// Values above this are clamped at resolve time. 300% → slate up to
/// `ceil(limit * 4.0)` before the absolute `MAX_RERANK_SLATE` cap applies.
pub const MAX_RERANKER_OVER_FETCH_PERCENT: u32 = 300;

/// Fully resolved reranker settings when both URL and model are available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedReranker {
    pub url: String,
    pub model: String,
    /// Extra percent beyond the final result limit for the pre-rerank slate.
    /// Slate size = `min(ceil(limit * (100 + p) / 100), MAX_RERANK_SLATE)`.
    /// Example: 50 with limit 10 → 15 candidates.
    pub over_fetch_percent: u32,
    pub max_doc_chars: usize,
    pub max_query_chars: usize,
    pub profile_id: Option<&'static str>,
}

/// Compute the candidate slate size for reranking.
///
/// `over_fetch_percent` is **extra** percent beyond `limit` (not a multiplier).
/// - limit=10, percent=50 → 15
/// - limit=10, percent=200 → 30
/// - limit=10, percent=0 → 10
///
/// Result is always at least `limit` (when limit > 0) and at most `max_slate`.
pub fn rerank_slate_size(limit: usize, over_fetch_percent: u32, max_slate: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let max_slate = max_slate.max(1);
    let inflated = (limit as u64)
        .saturating_mul(100u64.saturating_add(u64::from(over_fetch_percent)))
        .div_ceil(100) as usize;
    inflated.max(limit).min(max_slate)
}

/// Built-in catalog. First entry is the default when a profile is requested by default paths.
pub static RERANKER_PROFILES: &[RerankerProfile] = &[
    RerankerProfile {
        id: "jina-v2",
        model: DEFAULT_RERANKER_MODEL,
        max_doc_chars: JINA_MAX_DOC_CHARS,
        max_query_chars: JINA_MAX_QUERY_CHARS,
        hf_repo: Some("gpustack/jina-reranker-v2-base-multilingual-GGUF"),
        hf_file: Some("jina-reranker-v2-base-multilingual-FP16.gguf"),
        container_model_path: Some("/models/jina-reranker-v2.gguf"),
        notes: "Default. ~0.5 GB FP16, 1024-token ctx. Baked into docker/llama-box image.",
    },
    RerankerProfile {
        id: "nemotron-rerank-1b",
        // llama-box advertises the GGUF basename (see jina: "jina-reranker-v2.gguf").
        // Operators should bake/serve as llama-nemotron-rerank-1b-v2-q8_0.gguf.
        model: "llama-nemotron-rerank-1b-v2-q8_0.gguf",
        // 8192-token training ctx; char caps leave margin for ~0.25–0.4 tok/char English.
        max_doc_chars: 6000,
        max_query_chars: 1024,
        hf_repo: Some("kread/llama-nemotron-rerank-1b-v2-GGUF"),
        hf_file: Some("llama-nemotron-rerank-1b-v2-q8_0.gguf"),
        container_model_path: Some("/models/llama-nemotron-rerank-1b-v2-q8_0.gguf"),
        notes: "Nemotron rerank 1B Q8_0 (~1.3 GB), 8192-token ctx. Opt-in docker bake; ~2 GB VRAM.",
    },
];

/// Look up a profile by id (case-sensitive).
pub fn find_reranker_profile(id: &str) -> Option<&'static RerankerProfile> {
    RERANKER_PROFILES.iter().find(|p| p.id == id)
}

/// Default profile (`jina-v2`).
pub fn default_reranker_profile() -> &'static RerankerProfile {
    find_reranker_profile(DEFAULT_RERANKER_PROFILE_ID)
        .expect("default reranker profile must exist in catalog")
}

/// Resolve optional reranker settings.
///
/// Returns `Ok(None)` when neither URL nor model/profile is set (reranker disabled).
/// Returns `Err` for unknown profile ids.
/// Returns `Ok(None)` with the caller's responsibility to warn when only one of
/// URL/model is present — this function treats incomplete pairs as `None` after
/// resolving the model side, matching historical "both must be set" semantics
/// when the caller passes the pair check.
///
/// Call [`resolve_reranker_pair`] when you already know URL + model intent.
pub fn resolve_reranker_pair(
    reranker_url: Option<&str>,
    reranker_profile: Option<&str>,
    reranker_model: Option<&str>,
    reranker_over_fetch_percent: Option<u32>,
) -> Result<Option<ResolvedReranker>, String> {
    let url = reranker_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string());

    let (model, max_doc, max_query, profile_id) =
        resolve_model_side(reranker_profile, reranker_model)?;

    match (url, model) {
        (Some(url), Some(model)) => {
            // 0% extra is valid (slate == limit). Unset → default 200%.
            // Values above MAX_RERANKER_OVER_FETCH_PERCENT are clamped.
            let raw =
                reranker_over_fetch_percent.unwrap_or(DEFAULT_RERANKER_OVER_FETCH_PERCENT);
            let over_fetch_percent = raw.min(MAX_RERANKER_OVER_FETCH_PERCENT);
            Ok(Some(ResolvedReranker {
                url,
                model,
                over_fetch_percent,
                max_doc_chars: max_doc,
                max_query_chars: max_query,
                profile_id,
            }))
        }
        (None, None) => Ok(None),
        // Incomplete: only one side — signal as None; ProjectConfig warns.
        _ => Ok(None),
    }
}

fn resolve_model_side(
    reranker_profile: Option<&str>,
    reranker_model: Option<&str>,
) -> Result<(Option<String>, usize, usize, Option<&'static str>), String> {
    let default_caps = (
        JINA_MAX_DOC_CHARS,
        JINA_MAX_QUERY_CHARS,
    );

    if let Some(profile_id) = reranker_profile.map(str::trim).filter(|s| !s.is_empty()) {
        let profile = find_reranker_profile(profile_id).ok_or_else(|| {
            let known: Vec<&str> = RERANKER_PROFILES.iter().map(|p| p.id).collect();
            format!(
                "unknown rerankerProfile '{profile_id}'; known: {}",
                known.join(", ")
            )
        })?;
        return Ok((
            Some(profile.model.to_string()),
            profile.max_doc_chars,
            profile.max_query_chars,
            Some(profile.id),
        ));
    }

    if let Some(model) = reranker_model.map(str::trim).filter(|s| !s.is_empty()) {
        // Match catalog by model string (or container basename) for caps.
        if let Some(profile) = RERANKER_PROFILES.iter().find(|p| {
            p.model == model
                || p.container_model_path
                    .is_some_and(|path| path.ends_with(model) || path == model)
        }) {
            return Ok((
                Some(model.to_string()),
                profile.max_doc_chars,
                profile.max_query_chars,
                Some(profile.id),
            ));
        }
        let (d, q) = default_caps;
        return Ok((Some(model.to_string()), d, q, None));
    }

    Ok((None, default_caps.0, default_caps.1, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_set_disables() {
        assert!(resolve_reranker_pair(None, None, None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn jina_profile_with_url() {
        let r = resolve_reranker_pair(
            Some("http://localhost:8181"),
            Some("jina-v2"),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.model, DEFAULT_RERANKER_MODEL);
        assert_eq!(r.max_doc_chars, 1024);
        assert_eq!(r.max_query_chars, 256);
        assert_eq!(r.over_fetch_percent, DEFAULT_RERANKER_OVER_FETCH_PERCENT);
        assert_eq!(r.profile_id, Some("jina-v2"));
    }

    #[test]
    fn nemotron_profile_larger_caps() {
        let r = resolve_reranker_pair(
            Some("http://localhost:8181"),
            Some("nemotron-rerank-1b"),
            Some("ignored"),
            Some(50),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.profile_id, Some("nemotron-rerank-1b"));
        assert!(r.model.contains("nemotron"));
        assert_eq!(r.max_doc_chars, 6000);
        assert_eq!(r.max_query_chars, 1024);
        assert_eq!(r.over_fetch_percent, 50);
    }

    #[test]
    fn raw_model_zero_percent_ok() {
        let r = resolve_reranker_pair(
            Some("http://localhost:8181"),
            None,
            Some("custom-rerank"),
            Some(0),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.model, "custom-rerank");
        assert_eq!(r.max_doc_chars, JINA_MAX_DOC_CHARS);
        assert_eq!(r.over_fetch_percent, 0);
        assert!(r.profile_id.is_none());
    }

    #[test]
    fn slate_size_extra_percent() {
        assert_eq!(rerank_slate_size(10, 50, 30), 15);
        assert_eq!(rerank_slate_size(10, 200, 30), 30);
        assert_eq!(rerank_slate_size(10, 0, 30), 10);
        assert_eq!(rerank_slate_size(20, 200, 30), 30); // capped by max_slate
        assert_eq!(rerank_slate_size(0, 50, 30), 0);
        // 300% extra → 4× limit before max_slate
        assert_eq!(rerank_slate_size(5, 300, 30), 20);
        assert_eq!(rerank_slate_size(10, 300, 30), 30);
    }

    #[test]
    fn over_fetch_percent_clamped_at_max() {
        let r = resolve_reranker_pair(
            Some("http://localhost:8181"),
            Some("jina-v2"),
            None,
            Some(999),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.over_fetch_percent, MAX_RERANKER_OVER_FETCH_PERCENT);
        assert_eq!(MAX_RERANKER_OVER_FETCH_PERCENT, 300);
    }

    #[test]
    fn over_fetch_percent_at_max_boundary_not_clamped_further() {
        let r = resolve_reranker_pair(
            Some("http://localhost:8181"),
            Some("jina-v2"),
            None,
            Some(300),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.over_fetch_percent, 300);
    }

    #[test]
    fn default_over_fetch_is_two_hundred() {
        assert_eq!(DEFAULT_RERANKER_OVER_FETCH_PERCENT, 200);
        assert!(DEFAULT_RERANKER_OVER_FETCH_PERCENT <= MAX_RERANKER_OVER_FETCH_PERCENT);
    }

    #[test]
    fn unknown_profile_errors() {
        let err = resolve_reranker_pair(Some("http://x"), Some("nope"), None, None).unwrap_err();
        assert!(err.contains("unknown rerankerProfile"));
    }

    #[test]
    fn incomplete_url_only_is_none() {
        assert!(
            resolve_reranker_pair(Some("http://localhost:8181"), None, None, None)
                .unwrap()
                .is_none()
        );
    }
}
