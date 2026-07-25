//! Built-in embedding model profiles for recall / curate embed.
//!
//! Profiles are the SSoT for Ollama model ids, expected dimensions, and the
//! query/passage prefixes required by some models (e.g. Nemotron-3-Embed).
//! Operators select a profile via `embeddingProfile` in `.task-mgr/config.json`,
//! or pass a raw `embeddingModel` string (escape hatch: no prefixes).

/// Default embedding profile id (Jina small Q8_0).
pub const DEFAULT_EMBEDDING_PROFILE_ID: &str = "jina-small-q8";

/// One catalog entry for an embedding backend served by Ollama.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingProfile {
    /// Stable id used in config (`embeddingProfile`).
    pub id: &'static str,
    /// Model name as known to Ollama (`/api/tags`, `/api/embed`).
    pub ollama_model: &'static str,
    /// Expected embedding dimensionality, when known.
    pub expected_dims: Option<usize>,
    /// Prepended to document/passage text before embed (may be empty).
    pub passage_prefix: &'static str,
    /// Prepended to query text before embed (may be empty).
    pub query_prefix: &'static str,
    /// Short human description for `--list-profiles` / docs.
    pub notes: &'static str,
}

/// Fully resolved embedding settings for a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEmbedding {
    pub ollama_url: String,
    pub model: String,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub expected_dims: Option<usize>,
    /// Catalog id when resolved from a profile; `None` for raw model strings.
    pub profile_id: Option<&'static str>,
}

/// Built-in catalog. First entry is the default.
pub static EMBEDDING_PROFILES: &[EmbeddingProfile] = &[
    EmbeddingProfile {
        id: "jina-small-q8",
        ollama_model: "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0",
        expected_dims: Some(1024),
        passage_prefix: "",
        query_prefix: "",
        notes: "Default. Small (~0.6 GB), 1024-d. Fast on CPU or GPU.",
    },
    EmbeddingProfile {
        id: "nemotron-3-embed-q8",
        ollama_model: "hf.co/Aqua00/Nemotron-3-Embed-8B-GGUF:Q8_0",
        expected_dims: Some(4096),
        passage_prefix: "passage: ",
        query_prefix: "query: ",
        notes: "Nemotron-3-Embed 8B Q8_0 (~8.5 GB weights, 4096-d). Needs ~9–11 GB VRAM; re-embed after switch.",
    },
];

/// Look up a profile by id (case-sensitive).
pub fn find_embedding_profile(id: &str) -> Option<&'static EmbeddingProfile> {
    EMBEDDING_PROFILES.iter().find(|p| p.id == id)
}

/// Default profile (`jina-small-q8`).
pub fn default_embedding_profile() -> &'static EmbeddingProfile {
    find_embedding_profile(DEFAULT_EMBEDDING_PROFILE_ID)
        .expect("default embedding profile must exist in catalog")
}

/// Apply a role prefix if non-empty.
pub fn format_embed_input(prefix: &str, text: &str) -> String {
    if prefix.is_empty() {
        text.to_string()
    } else {
        format!("{prefix}{text}")
    }
}

/// Resolve embedding settings from optional config fields.
///
/// Precedence:
/// 1. `embedding_profile` → catalog (fills model + prefixes + dims)
/// 2. Else `embedding_model` raw string → no prefixes, unknown dims
/// 3. Else default profile (`jina-small-q8`)
///
/// `ollama_url` falls back to [`super::DEFAULT_OLLAMA_URL`].
///
/// Unknown profile ids return `Err` with a helpful message listing known ids.
pub fn resolve_embedding(
    ollama_url: Option<&str>,
    embedding_profile: Option<&str>,
    embedding_model: Option<&str>,
) -> Result<ResolvedEmbedding, String> {
    let url = ollama_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(super::DEFAULT_OLLAMA_URL)
        .trim_end_matches('/')
        .to_string();

    if let Some(profile_id) = embedding_profile.map(str::trim).filter(|s| !s.is_empty()) {
        let profile = find_embedding_profile(profile_id).ok_or_else(|| {
            let known: Vec<&str> = EMBEDDING_PROFILES.iter().map(|p| p.id).collect();
            format!(
                "unknown embeddingProfile '{profile_id}'; known: {}",
                known.join(", ")
            )
        })?;
        return Ok(ResolvedEmbedding {
            ollama_url: url,
            model: profile.ollama_model.to_string(),
            query_prefix: profile.query_prefix.to_string(),
            passage_prefix: profile.passage_prefix.to_string(),
            expected_dims: profile.expected_dims,
            profile_id: Some(profile.id),
        });
    }

    if let Some(model) = embedding_model.map(str::trim).filter(|s| !s.is_empty()) {
        // Raw escape hatch: if the string matches a catalog ollama_model, adopt
        // that profile's prefixes so operators who only set embeddingModel still
        // get correct Nemotron formatting.
        if let Some(profile) = EMBEDDING_PROFILES
            .iter()
            .find(|p| p.ollama_model == model)
        {
            return Ok(ResolvedEmbedding {
                ollama_url: url,
                model: model.to_string(),
                query_prefix: profile.query_prefix.to_string(),
                passage_prefix: profile.passage_prefix.to_string(),
                expected_dims: profile.expected_dims,
                profile_id: Some(profile.id),
            });
        }
        return Ok(ResolvedEmbedding {
            ollama_url: url,
            model: model.to_string(),
            query_prefix: String::new(),
            passage_prefix: String::new(),
            expected_dims: None,
            profile_id: None,
        });
    }

    let profile = default_embedding_profile();
    Ok(ResolvedEmbedding {
        ollama_url: url,
        model: profile.ollama_model.to_string(),
        query_prefix: profile.query_prefix.to_string(),
        passage_prefix: profile.passage_prefix.to_string(),
        expected_dims: profile.expected_dims,
        profile_id: Some(profile.id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learnings::embeddings::DEFAULT_EMBEDDING_MODEL;

    #[test]
    fn default_resolves_jina() {
        let r = resolve_embedding(None, None, None).unwrap();
        assert_eq!(r.profile_id, Some("jina-small-q8"));
        assert_eq!(r.model, DEFAULT_EMBEDDING_MODEL);
        assert!(r.query_prefix.is_empty());
        assert!(r.passage_prefix.is_empty());
        assert_eq!(r.expected_dims, Some(1024));
        assert_eq!(r.ollama_url, "http://localhost:11435");
    }

    #[test]
    fn profile_nemotron_applies_prefixes() {
        let r = resolve_embedding(None, Some("nemotron-3-embed-q8"), None).unwrap();
        assert_eq!(r.profile_id, Some("nemotron-3-embed-q8"));
        assert_eq!(r.query_prefix, "query: ");
        assert_eq!(r.passage_prefix, "passage: ");
        assert_eq!(r.expected_dims, Some(4096));
        assert!(r.model.contains("Nemotron-3-Embed-8B"));
    }

    #[test]
    fn raw_model_escape_hatch() {
        let r = resolve_embedding(None, None, Some("my-custom-model")).unwrap();
        assert_eq!(r.model, "my-custom-model");
        assert!(r.profile_id.is_none());
        assert!(r.query_prefix.is_empty());
        assert_eq!(r.expected_dims, None);
    }

    #[test]
    fn raw_model_matching_catalog_adopts_prefixes() {
        let nem = find_embedding_profile("nemotron-3-embed-q8").unwrap();
        let r = resolve_embedding(None, None, Some(nem.ollama_model)).unwrap();
        assert_eq!(r.profile_id, Some("nemotron-3-embed-q8"));
        assert_eq!(r.query_prefix, "query: ");
    }

    #[test]
    fn profile_wins_over_raw_model() {
        let r = resolve_embedding(
            Some("http://example:9"),
            Some("jina-small-q8"),
            Some("should-be-ignored"),
        )
        .unwrap();
        assert_eq!(r.profile_id, Some("jina-small-q8"));
        assert_eq!(r.model, DEFAULT_EMBEDDING_MODEL);
        assert_eq!(r.ollama_url, "http://example:9");
    }

    #[test]
    fn unknown_profile_errors() {
        let err = resolve_embedding(None, Some("nope"), None).unwrap_err();
        assert!(err.contains("unknown embeddingProfile"));
        assert!(err.contains("jina-small-q8"));
    }

    #[test]
    fn format_embed_input_empty_prefix() {
        assert_eq!(format_embed_input("", "hello"), "hello");
    }

    #[test]
    fn format_embed_input_with_prefix() {
        assert_eq!(format_embed_input("query: ", "hello"), "query: hello");
    }
}
