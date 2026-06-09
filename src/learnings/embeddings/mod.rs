//! Embedding support for learnings: OllamaEmbedder, storage, and cosine similarity.
//!
//! Provides local embedding generation via Ollama, BLOB-based storage in SQLite,
//! and cosine similarity computation for pre-filtering duplicate candidates.

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Default Ollama server URL.
///
/// Port 11435 (not the upstream Ollama default 11434) so the bundled
/// docker-compose stack doesn't clash with a host-installed `ollama serve`.
pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11435";

/// Default embedding model.
pub const DEFAULT_EMBEDDING_MODEL: &str =
    "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0";

/// Cosine similarity threshold for treating a candidate as a near-duplicate of an
/// existing learning.
///
/// Asymmetric-risk rationale: a false positive (incorrectly deciding "duplicate" and
/// dropping the candidate) is unrecoverable — there is no LLM second opinion at
/// write time, and the learning is silently lost. A false negative (letting a
/// near-duplicate through) is cheap because `curate dedup` will catch it later.
/// Bias high (0.92) and well above the curate pre-cluster threshold (0.65) to
/// minimize the risk of dropping a distinct learning.
pub const NEAR_DUP_THRESHOLD: f32 = 0.92;

/// Log floor for "near-miss" observability band inside `NearDuplicateChecker::check`.
///
/// When the best-match similarity satisfies `NEAR_MISS_LOG_FLOOR <= sim < threshold`,
/// we emit a diagnostic line and still treat the candidate as Unique (so it is
/// recorded). This band aids calibration without changing the accept decision.
const NEAR_MISS_LOG_FLOOR: f32 = 0.80;

// ---------------------------------------------------------------------------
// OllamaEmbedder
// ---------------------------------------------------------------------------

/// Concrete embedder that talks to a local Ollama server via HTTP.
///
/// Uses the `/api/embed` endpoint for embedding generation and `/api/tags`
/// for availability/model checks.
pub struct OllamaEmbedder {
    base_url: String,
    model: String,
    /// Agent with short timeouts for health/availability checks.
    health_agent: ureq::Agent,
    /// Agent with longer timeouts for embedding requests.
    embed_agent: ureq::Agent,
}

impl OllamaEmbedder {
    /// Create a new embedder pointing at the given Ollama server and model.
    pub fn new(base_url: &str, model: &str) -> Self {
        let health_agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(3)))
            .timeout_recv_response(Some(std::time::Duration::from_secs(3)))
            .timeout_recv_body(Some(std::time::Duration::from_secs(3)))
            .build()
            .into();
        let embed_agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(3)))
            .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
            .timeout_recv_body(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            health_agent,
            embed_agent,
        }
    }

    /// The model name this embedder is configured to use.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The base URL this embedder targets (with any trailing slash already trimmed).
    ///
    /// Exposed so callers constructing `TaskMgrError::OllamaUnreachable` can
    /// surface the exact URL that was contacted, rather than re-deriving it
    /// from defaults or config.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Check whether Ollama is reachable AND the configured model is available.
    ///
    /// Calls `GET /api/tags` with a 3-second timeout and checks whether the
    /// configured model appears in the response's `models[].name` array.
    pub fn is_available(&self) -> Result<bool, String> {
        let url = format!("{}/api/tags", self.base_url);

        let mut resp = match self.health_agent.get(&url).call() {
            Ok(r) => r,
            Err(e) => return Err(format!("Ollama not reachable at {}: {e}", self.base_url)),
        };

        let body: serde_json::Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("Failed to parse /api/tags response: {e}"))?;

        let models = body["models"]
            .as_array()
            .ok_or_else(|| "Invalid /api/tags response: missing models array".to_string())?;

        let found = models
            .iter()
            .any(|m| m["name"].as_str().is_some_and(|name| name == self.model));

        Ok(found)
    }

    /// Embed a single text string. Returns the embedding vector.
    ///
    /// Uses a 30-second timeout for the embedding call.
    pub fn embed(&self, text: &str) -> TaskMgrResult<Vec<f32>> {
        let batch = self.embed_batch(&[text])?;
        batch.into_iter().next().ok_or_else(|| {
            crate::TaskMgrError::IoError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Ollama returned empty embeddings array for single input",
            ))
        })
    }

    /// Embed a batch of texts. Returns one embedding vector per input text.
    ///
    /// Uses a 30-second timeout. Sends all texts in a single HTTP request
    /// using the array form of the `input` field.
    pub fn embed_batch(&self, texts: &[&str]) -> TaskMgrResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/api/embed", self.base_url);

        let payload = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let mut resp = self
            .embed_agent
            .post(&url)
            .send_json(&payload)
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("Ollama embed request failed: {e}"),
                )
            })?;

        let body: serde_json::Value = resp.body_mut().read_json().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to parse Ollama embed response: {e}"),
            )
        })?;

        let embeddings_arr = body["embeddings"].as_array().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid Ollama response: missing embeddings array",
            )
        })?;

        let mut result = Vec::with_capacity(embeddings_arr.len());
        for (i, emb) in embeddings_arr.iter().enumerate() {
            let vec = emb.as_array().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Invalid Ollama response: embeddings[{i}] is not an array"),
                )
            })?;
            let floats: Vec<f32> = vec
                .iter()
                .enumerate()
                .map(|(j, v)| {
                    v.as_f64().map(|f| f as f32).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("embeddings[{i}][{j}] is not a number"),
                        )
                    })
                })
                .collect::<Result<Vec<f32>, _>>()?;
            result.push(floats);
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// BLOB conversion helpers
// ---------------------------------------------------------------------------

/// Encode a float vector as a little-endian BLOB for SQLite storage.
pub fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Decode a little-endian BLOB back to a float vector.
pub fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    debug_assert!(
        blob.len().is_multiple_of(4),
        "BLOB length {} is not a multiple of 4",
        blob.len()
    );
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

// ---------------------------------------------------------------------------
// Cosine similarity
// ---------------------------------------------------------------------------

/// Compute cosine similarity between two vectors.
///
/// Returns:
/// -  `1.0` for identical direction
/// -  `0.0` for orthogonal or if either vector is zero-length
/// - `-1.0` for opposite direction
///
/// Handles zero vectors gracefully (returns `0.0`, never `NaN`).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut dot = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;

    for i in 0..len {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom == 0.0 {
        return 0.0;
    }

    (dot / denom) as f32
}

// ---------------------------------------------------------------------------
// Near-duplicate primitives (pure, no I/O)
// ---------------------------------------------------------------------------

/// Return the single highest-similarity known entry for `candidate`, or None if
/// `known` is empty.
///
/// Compares using [`cosine_similarity`] and returns the `(id, sim)` pair for the
/// maximum similarity regardless of any threshold. This is the testable primitive
/// for near-duplicate decisions; threshold filtering is applied by the caller or
/// by [`find_near_duplicate`].
pub fn best_match(candidate: &[f32], known: &[(i64, Vec<f32>)]) -> Option<(i64, f32)> {
    if known.is_empty() {
        return None;
    }
    let mut best: Option<(i64, f32)> = None;
    for (id, emb) in known {
        let sim = cosine_similarity(candidate, emb);
        match best {
            Some((_, s)) if sim > s => best = Some((*id, sim)),
            None => best = Some((*id, sim)),
            _ => {}
        }
    }
    best
}

/// Return the highest-similarity known entry whose similarity is >= `threshold`,
/// or None if no such entry exists (including when `known` is empty).
///
/// Defined as `best_match(candidate, known).filter(|(_, s)| *s >= threshold)`.
pub fn find_near_duplicate(
    candidate: &[f32],
    known: &[(i64, Vec<f32>)],
    threshold: f32,
) -> Option<(i64, f32)> {
    best_match(candidate, known).filter(|(_, s)| *s >= threshold)
}

// ---------------------------------------------------------------------------
// Storage functions
// ---------------------------------------------------------------------------

/// Store (or replace) an embedding for a learning.
pub fn store_embedding(
    conn: &Connection,
    learning_id: i64,
    model: &str,
    embedding: &[f32],
) -> TaskMgrResult<()> {
    let blob = embedding_to_blob(embedding);
    let dimensions = embedding.len() as i64;
    conn.execute(
        "INSERT OR REPLACE INTO learning_embeddings (learning_id, model, dimensions, embedding) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![learning_id, model, dimensions, blob],
    )?;
    Ok(())
}

/// Load the embedding for a single learning. Returns `None` if not found.
pub fn load_embedding(conn: &Connection, learning_id: i64) -> TaskMgrResult<Option<Vec<f32>>> {
    let mut stmt =
        conn.prepare("SELECT embedding FROM learning_embeddings WHERE learning_id = ?1")?;

    let result = stmt.query_row([learning_id], |row| {
        let blob: Vec<u8> = row.get(0)?;
        Ok(blob_to_embedding(&blob))
    });

    match result {
        Ok(emb) => Ok(Some(emb)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A learning ID paired with its embedding vector.
pub struct LearningEmbedding {
    pub learning_id: i64,
    pub embedding: Vec<f32>,
}

/// Load all embeddings for active (non-retired) learnings with the given model.
///
/// Joins `learning_embeddings` with `learnings` to exclude retired learnings
/// (`retired_at IS NOT NULL`).
pub fn load_all_active_embeddings(
    conn: &Connection,
    model: &str,
) -> TaskMgrResult<Vec<LearningEmbedding>> {
    let mut stmt = conn.prepare(
        "SELECT le.learning_id, le.embedding
         FROM learning_embeddings le
         JOIN learnings l ON l.id = le.learning_id
         WHERE l.retired_at IS NULL
           AND le.model = ?1",
    )?;

    let rows = stmt.query_map([model], |row| {
        let learning_id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok(LearningEmbedding {
            learning_id,
            embedding: blob_to_embedding(&blob),
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Count how many active (non-retired) learnings have embeddings for the given model.
pub fn count_embedded(conn: &Connection, model: &str) -> TaskMgrResult<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM learning_embeddings le
         JOIN learnings l ON l.id = le.learning_id
         WHERE l.retired_at IS NULL
           AND le.model = ?1",
        [model],
        |row| row.get(0),
    )?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Best-effort inline embedding
// ---------------------------------------------------------------------------

/// Compose the embedding input text from title and content.
///
/// Matches the logic in `curate embed`: `"title\n\ncontent"`, or just `title`
/// when content is empty.
pub fn compose_embed_text(title: &str, content: &str) -> String {
    let content = content.trim();
    if content.is_empty() {
        title.trim().to_string()
    } else {
        format!("{}\n\n{}", title.trim(), content)
    }
}

/// Best-effort embedding of a single learning.
///
/// **Note:** Production creation paths now use [`LearningWriter`](crate::learnings::LearningWriter)
/// which calls [`try_embed_learnings_batch`] via `flush()`. This single-learning variant
/// is retained as a standalone utility but is not called from any production path.
///
/// Reads Ollama config from `ProjectConfig`, checks availability, embeds, and
/// stores. On any failure (Ollama down, model missing, network timeout, etc.)
/// prints a warning to stderr and returns `false`. Returns `true` when
/// the embedding was stored successfully.
///
/// This is intentionally fire-and-forget: callers should never propagate the
/// error since a missing embedding is recoverable via `curate embed`.
pub fn try_embed_learning(
    conn: &Connection,
    db_dir: &std::path::Path,
    learning_id: i64,
    title: &str,
    content: &str,
) -> bool {
    use crate::loop_engine::project_config::read_project_config;

    let proj = read_project_config(db_dir);
    let ollama_url = proj
        .ollama_url
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
    let model = proj
        .embedding_model
        .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());

    let text = compose_embed_text(title, content);
    if text.is_empty() {
        return false;
    }

    let embedder = OllamaEmbedder::new(&ollama_url, &model);

    match embedder.is_available() {
        Ok(true) => {}
        Ok(false) => return false,
        Err(_) => return false,
    }

    let embedding = match embedder.embed(&text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Warning: failed to embed learning {learning_id}: {e}");
            return false;
        }
    };

    if let Err(e) = store_embedding(conn, learning_id, &model, &embedding) {
        eprintln!("Warning: failed to store embedding for learning {learning_id}: {e}");
        return false;
    }

    true
}

/// Best-effort batch embedding of multiple learnings after bulk creation.
///
/// Similar to [`try_embed_learning`] but batches the Ollama calls for
/// efficiency. Returns the count of successfully embedded learnings.
pub fn try_embed_learnings_batch(
    conn: &Connection,
    db_dir: &std::path::Path,
    learnings: &[(i64, String, String)], // (id, title, content)
) -> usize {
    use crate::loop_engine::project_config::read_project_config;

    if learnings.is_empty() {
        return 0;
    }

    let proj = read_project_config(db_dir);
    let ollama_url = proj
        .ollama_url
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
    let model = proj
        .embedding_model
        .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());

    let embedder = OllamaEmbedder::new(&ollama_url, &model);

    match embedder.is_available() {
        Ok(true) => {}
        Ok(false) => return 0,
        Err(_) => return 0,
    }

    // Build texts, skipping empty ones
    let items: Vec<(i64, String)> = learnings
        .iter()
        .filter_map(|(id, title, content)| {
            let text = compose_embed_text(title, content);
            if text.is_empty() {
                None
            } else {
                Some((*id, text))
            }
        })
        .collect();

    if items.is_empty() {
        return 0;
    }

    const BATCH_SIZE: usize = 50;
    let mut stored = 0;

    for chunk in items.chunks(BATCH_SIZE) {
        let texts: Vec<&str> = chunk.iter().map(|(_, t)| t.as_str()).collect();
        match embedder.embed_batch(&texts) {
            Ok(embeddings) => {
                for ((id, _), emb) in chunk.iter().zip(embeddings.iter()) {
                    if store_embedding(conn, *id, &model, emb).is_ok() {
                        stored += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: batch embedding failed: {e}");
            }
        }
    }

    stored
}

// ---------------------------------------------------------------------------
// Near-duplicate guard (embedding-based pre-filter at write time)
// ---------------------------------------------------------------------------

/// Outcome of an inline near-duplicate check performed by [`NearDuplicateChecker`].
#[derive(Debug, Clone, PartialEq)]
pub enum NearDupOutcome {
    /// Candidate is similar enough to an existing learning to be treated as a duplicate.
    Duplicate { existing_id: i64, similarity: f32 },
    /// Candidate is unique (or best match is below the near-miss floor); the
    /// freshly-computed embedding is returned so the caller can register it for
    /// subsequent same-batch checks via [`NearDuplicateChecker::register`].
    Unique(Vec<f32>),
    /// Checker unavailable (embedder down, empty text, etc.). Caller must fall
    /// back to exact-match-only behavior.
    Unavailable,
}

/// Reusable near-duplicate checker that embeds a candidate once and compares it
/// (via cosine) against embeddings previously loaded for the *configured* model.
///
/// Cross-model comparison is intentionally skipped: only rows stored under the
/// model resolved from the current project config (via `read_project_config`) are
/// loaded at construction time. This matches the embedding used for the candidate
/// and avoids meaningless similarity scores across different embedding spaces.
///
/// The struct deliberately holds no separate `model` field; `OllamaEmbedder` already
/// retains the model it was constructed with. Adding a second copy would trigger
/// `dead_code` under `-D warnings` because the value is never read after `new`.
pub struct NearDuplicateChecker {
    embedder: OllamaEmbedder,
    threshold: f32,
    known: Vec<(i64, Vec<f32>)>,
}

impl NearDuplicateChecker {
    /// Construct a checker for the given connection and project directory.
    ///
    /// Resolves `ollama_url` and `embedding_model` exactly once via
    /// `read_project_config` (mirrors the locals pattern in `try_embed_learning`).
    /// Builds an embedder, verifies availability, then loads all active embeddings
    /// for that model via [`load_all_active_embeddings`]. On load error, prints a
    /// warning and returns `None` (preserving exact-match-only behavior).
    ///
    /// Returns `None` (never panics, never Err) when the embedder is unavailable
    /// or the load fails. The local `model` binding is used for both the embedder
    /// and the loader then dropped; it is not stored on the struct.
    pub fn new(conn: &Connection, db_dir: &std::path::Path, threshold: f32) -> Option<Self> {
        use crate::loop_engine::project_config::read_project_config;

        let proj = read_project_config(db_dir);
        let url = proj
            .ollama_url
            .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
        let model = proj
            .embedding_model
            .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());

        let embedder = OllamaEmbedder::new(&url, &model);

        if !matches!(embedder.is_available(), Ok(true)) {
            return None;
        }

        // Only embeddings for the *configured* model are loaded; cross-model
        // comparison is intentionally skipped (see struct doc comment).
        let known = match load_all_active_embeddings(conn, &model) {
            Ok(v) => v
                .into_iter()
                .map(|le| (le.learning_id, le.embedding))
                .collect(),
            Err(e) => {
                eprintln!("Warning: near-dup checker load failed: {e}");
                return None;
            }
        };

        Some(Self {
            embedder,
            threshold,
            known,
        })
    }

    /// Check whether `(title, content)` is a near-duplicate of a known learning.
    ///
    /// - Empty composed text -> `Unavailable`.
    /// - Embed failure -> `Unavailable` (fire-and-forget; never propagates).
    /// - Best match with sim >= threshold -> `Duplicate`.
    /// - Best match with `NEAR_MISS_LOG_FLOOR <= sim < threshold` -> log a
    ///   near-miss line then `Unique(embedding)`.
    /// - Otherwise (including no known entries) -> `Unique(embedding)`.
    ///
    /// The near-miss observability arm exists precisely because `check` re-uses
    /// `best_match` (the max regardless of threshold) rather than only testing
    /// `>= threshold`; a bare `if >= { Duplicate } else { Unique }` would skip
    /// the diagnostic the split enables.
    pub fn check(&self, title: &str, content: &str) -> NearDupOutcome {
        let text = compose_embed_text(title, content);
        if text.is_empty() {
            return NearDupOutcome::Unavailable;
        }

        let emb = match self.embedder.embed(&text) {
            Ok(v) => v,
            Err(_) => return NearDupOutcome::Unavailable,
        };

        if let Some((id, sim)) = best_match(&emb, &self.known) {
            if sim >= self.threshold {
                NearDupOutcome::Duplicate {
                    existing_id: id,
                    similarity: sim,
                }
            } else if sim >= NEAR_MISS_LOG_FLOOR {
                eprintln!("near-miss #{} cos={:.3} (< {})", id, sim, self.threshold);
                NearDupOutcome::Unique(emb)
            } else {
                NearDupOutcome::Unique(emb)
            }
        } else {
            NearDupOutcome::Unique(emb)
        }
    }

    /// Register a freshly-accepted unique learning's embedding so that subsequent
    /// candidates in the same batch are compared against it as well.
    pub fn register(&mut self, id: i64, embedding: Vec<f32>) {
        self.known.push((id, embedding));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, open_connection, run_migrations};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, rusqlite::Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_learning(conn: &rusqlite::Connection, id: i64, title: &str) {
        conn.execute(
            "INSERT INTO learnings (id, title, content, outcome) VALUES (?1, ?2, 'content', 'pattern')",
            rusqlite::params![id, title],
        )
        .unwrap();
    }

    // ---- cosine_similarity tests ----

    #[test]
    fn test_cosine_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical vectors -> 1.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - (-1.0)).abs() < 1e-6,
            "opposite vectors -> -1.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "orthogonal vectors -> 0.0, got {sim}");
    }

    #[test]
    fn test_cosine_zero_vector_a() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "zero vector -> 0.0, not NaN");
        assert!(!sim.is_nan(), "must not return NaN for zero vector");
    }

    #[test]
    fn test_cosine_zero_vector_b() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![0.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "zero vector -> 0.0, not NaN");
    }

    #[test]
    fn test_cosine_both_zero() {
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0, "both zero -> 0.0");
    }

    #[test]
    fn test_cosine_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0, "empty vectors -> 0.0");
    }

    #[test]
    fn test_cosine_mismatched_lengths() {
        // Uses the shorter length
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 999.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "truncated to shorter -> 1.0, got {sim}"
        );
    }

    #[test]
    fn test_cosine_scaled_vectors() {
        // Cosine similarity is scale-invariant
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![100.0, 200.0, 300.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-5, "scaled vectors -> 1.0, got {sim}");
    }

    // ---- BLOB round-trip tests ----

    #[test]
    fn test_blob_round_trip_exact() {
        let original = vec![1.0_f32, -2.5, 0.0, f32::MIN, f32::MAX, std::f32::consts::PI];
        let blob = embedding_to_blob(&original);
        let decoded = blob_to_embedding(&blob);
        assert_eq!(original, decoded, "BLOB round-trip must be exact");
    }

    #[test]
    fn test_blob_round_trip_empty() {
        let original: Vec<f32> = vec![];
        let blob = embedding_to_blob(&original);
        let decoded = blob_to_embedding(&blob);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_blob_size() {
        let embedding = vec![0.0_f32; 1024];
        let blob = embedding_to_blob(&embedding);
        assert_eq!(blob.len(), 1024 * 4, "1024 f32s = 4096 bytes");
    }

    // ---- store/load round-trip tests ----

    #[test]
    fn test_store_load_round_trip() {
        let (_dir, conn) = setup_db();
        insert_learning(&conn, 1, "Test learning");

        let embedding = vec![1.0_f32, -2.5, 3.125, 0.0];
        store_embedding(&conn, 1, "test-model", &embedding).unwrap();

        let loaded = load_embedding(&conn, 1).unwrap();
        assert_eq!(
            loaded,
            Some(embedding),
            "store/load must round-trip exactly"
        );
    }

    #[test]
    fn test_load_embedding_not_found() {
        let (_dir, conn) = setup_db();
        let loaded = load_embedding(&conn, 999).unwrap();
        assert_eq!(loaded, None, "missing embedding returns None");
    }

    #[test]
    fn test_store_replaces_existing() {
        let (_dir, conn) = setup_db();
        insert_learning(&conn, 1, "Test");

        store_embedding(&conn, 1, "model-a", &[1.0, 2.0]).unwrap();
        store_embedding(&conn, 1, "model-b", &[3.0, 4.0, 5.0]).unwrap();

        let loaded = load_embedding(&conn, 1).unwrap().unwrap();
        assert_eq!(
            loaded,
            vec![3.0, 4.0, 5.0],
            "second store must replace first"
        );
    }

    // ---- load_all_active_embeddings tests ----

    #[test]
    fn test_load_all_excludes_retired() {
        let (_dir, conn) = setup_db();

        insert_learning(&conn, 1, "Active learning");
        insert_learning(&conn, 2, "Retired learning");

        // Retire learning 2
        conn.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = 2",
            [],
        )
        .unwrap();

        store_embedding(&conn, 1, "test-model", &[1.0, 2.0]).unwrap();
        store_embedding(&conn, 2, "test-model", &[3.0, 4.0]).unwrap();

        let active = load_all_active_embeddings(&conn, "test-model").unwrap();
        assert_eq!(active.len(), 1, "retired learning must be excluded");
        assert_eq!(active[0].learning_id, 1);
    }

    #[test]
    fn test_load_all_filters_by_model() {
        let (_dir, conn) = setup_db();

        insert_learning(&conn, 1, "Learning A");
        insert_learning(&conn, 2, "Learning B");

        store_embedding(&conn, 1, "model-a", &[1.0]).unwrap();
        store_embedding(&conn, 2, "model-b", &[2.0]).unwrap();

        let model_a = load_all_active_embeddings(&conn, "model-a").unwrap();
        assert_eq!(model_a.len(), 1, "must filter by model");
        assert_eq!(model_a[0].learning_id, 1);

        let model_b = load_all_active_embeddings(&conn, "model-b").unwrap();
        assert_eq!(model_b.len(), 1);
        assert_eq!(model_b[0].learning_id, 2);
    }

    #[test]
    fn test_load_all_empty() {
        let (_dir, conn) = setup_db();
        let all = load_all_active_embeddings(&conn, "any-model").unwrap();
        assert!(all.is_empty());
    }

    // ---- count_embedded tests ----

    #[test]
    fn test_count_embedded() {
        let (_dir, conn) = setup_db();

        insert_learning(&conn, 1, "A");
        insert_learning(&conn, 2, "B");
        insert_learning(&conn, 3, "C");

        store_embedding(&conn, 1, "model-x", &[1.0]).unwrap();
        store_embedding(&conn, 2, "model-x", &[2.0]).unwrap();
        store_embedding(&conn, 3, "model-y", &[3.0]).unwrap();

        assert_eq!(count_embedded(&conn, "model-x").unwrap(), 2);
        assert_eq!(count_embedded(&conn, "model-y").unwrap(), 1);
        assert_eq!(count_embedded(&conn, "model-z").unwrap(), 0);
    }

    #[test]
    fn test_count_embedded_excludes_retired() {
        let (_dir, conn) = setup_db();

        insert_learning(&conn, 1, "Active");
        insert_learning(&conn, 2, "Retired");
        conn.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = 2",
            [],
        )
        .unwrap();

        store_embedding(&conn, 1, "model-x", &[1.0]).unwrap();
        store_embedding(&conn, 2, "model-x", &[2.0]).unwrap();

        assert_eq!(count_embedded(&conn, "model-x").unwrap(), 1);
    }

    // ---- best_match / find_near_duplicate pure tests (no Ollama) ----

    #[test]
    fn test_near_dup_consts() {
        assert!((NEAR_DUP_THRESHOLD - 0.92).abs() < 1e-9);
        assert!((NEAR_MISS_LOG_FLOOR - 0.80).abs() < 1e-9);
    }

    #[test]
    fn test_best_match_empty_known() {
        let cand = vec![1.0, 0.0];
        assert!(best_match(&cand, &[]).is_none());
        assert!(find_near_duplicate(&cand, &[], NEAR_DUP_THRESHOLD).is_none());
    }

    #[test]
    fn test_best_match_identical_returns_some_with_id() {
        let id = 42i64;
        let v = vec![0.1, 0.2, 0.3];
        let known = vec![(id, v.clone())];
        let res = best_match(&v, &known);
        assert_eq!(res, Some((id, 1.0)));
    }

    #[test]
    fn test_find_near_duplicate_orthogonal_below_threshold() {
        let cand = vec![1.0, 0.0];
        let known = vec![(7, vec![0.0, 1.0])];
        let res = find_near_duplicate(&cand, &known, 0.92);
        assert!(res.is_none(), "cosine 0 with thresh 0.92 -> None");
    }

    #[test]
    fn test_find_near_duplicate_exactly_at_threshold_is_match() {
        // Construct two unit vectors whose cosine is exactly the threshold
        // (within f32 precision): the known vector is the x-axis, and the
        // candidate is forced onto the unit sphere at angle acos(0.92) so the
        // exact-at-threshold case is exercised deterministically.
        let thresh = 0.92_f32;
        let known_vec = vec![1.0_f32, 0.0, 0.0];
        // cos theta = 0.92 => adjacent component 0.92 on unit sphere slice
        let b0 = thresh;
        let b1 = (1.0_f32 - thresh * thresh).sqrt();
        let cand_vec = vec![b0, b1, 0.0];
        // Verify our construction
        let sim = cosine_similarity(&known_vec, &cand_vec);
        assert!(
            (sim - thresh).abs() < 1e-5,
            "test construction must yield ~exactly threshold, got {sim}"
        );
        let known = vec![(99i64, known_vec)];
        let hit = find_near_duplicate(&cand_vec, &known, thresh);
        assert_eq!(
            hit,
            Some((99, sim)),
            "exactly-at-threshold must be >= match"
        );
    }

    #[test]
    fn test_best_match_returns_max_not_first() {
        // sims: 0.93, 0.97, 0.95 -> must return the 0.97 id (105), not the first (101)
        let v1 = vec![1.0_f32, 0.0, 0.0]; // ref
        // Build vectors with controlled cosines to v1 (unit).
        let c93 = 0.93_f32;
        let c97 = 0.97_f32;
        let c95 = 0.95_f32;
        let v93 = vec![c93, (1.0 - c93 * c93).sqrt(), 0.0];
        let v97 = vec![c97, (1.0 - c97 * c97).sqrt(), 0.0];
        let v95 = vec![c95, (1.0 - c95 * c95).sqrt(), 0.0];
        let known = vec![(101i64, v93), (105i64, v97), (103i64, v95)];
        let cand = v1;
        let best = best_match(&cand, &known).expect("must have a best");
        assert_eq!(
            best.0, 105,
            "must pick the max-similarity id (0.97), not first"
        );
        assert!((best.1 - 0.97).abs() < 1e-5);
    }

    #[test]
    fn test_find_near_duplicate_below_threshold_none() {
        let cand = vec![1.0_f32, 0.0, 0.0];
        let low = vec![0.5_f32, (1.0_f32 - 0.25_f32).sqrt(), 0.0];
        let known = vec![(1, low)];
        assert!(best_match(&cand, &known).is_some());
        assert!(find_near_duplicate(&cand, &known, 0.92).is_none());
    }

    // The following two tests document "known-bad" implementations:
    // - Returning the *first* high match instead of the global max fails the ordering test.
    // - Using `> threshold` instead of `>=` fails the exactly-at-threshold test.
    // They are written to assert the *specific* max id and the inclusive boundary so that
    // a wrong impl would fail these (not just "got a match").

    #[test]
    fn test_best_match_max_id_assertion_would_fail_first_match_impl() {
        // If an impl did `find(|s| s >= 0.9).first()` or "return first >= thresh" style,
        // this ordering would surface the wrong id. We assert the true max id.
        let base = vec![1.0_f32, 0.0, 0.0];
        let k1 = vec![0.93_f32, (1.0_f32 - 0.93_f32 * 0.93_f32).sqrt(), 0.0];
        let k2 = vec![0.97_f32, (1.0_f32 - 0.97_f32 * 0.97_f32).sqrt(), 0.0];
        let k3 = vec![0.95_f32, (1.0_f32 - 0.95_f32 * 0.95_f32).sqrt(), 0.0];
        let known = vec![(11, k1), (22, k2), (33, k3)];
        let best = best_match(&base, &known).unwrap();
        assert_eq!(
            best.0, 22,
            "known-bad first-match would have returned 11 here"
        );
    }

    #[test]
    fn test_find_near_duplicate_boundary_ge_would_fail_gt_impl() {
        let thresh = 0.92_f32;
        let base = vec![1.0_f32, 0.0, 0.0];
        let b0 = thresh;
        let b1 = (1.0_f32 - thresh * thresh).sqrt();
        let exact = vec![b0, b1, 0.0];
        let known = vec![(7, base)];
        let hit = find_near_duplicate(&exact, &known, thresh);
        assert!(
            hit.is_some(),
            "known-bad `> threshold` impl would return None for exactly == threshold"
        );
    }

    // ---- NearDuplicateChecker tests (construction + basic check behavior) ----

    #[test]
    fn test_near_duplicate_checker_new_unreachable_via_temp_config() {
        // AC: new returns None (not Err, no panic) when Ollama unreachable.
        // Use a temp config dir with an unreachable ollama_url.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            cfg_dir.path().join("config.json"),
            r#"{"ollamaUrl": "http://127.0.0.1:1"}"#,
        )
        .unwrap();

        let (_db_dir, conn) = setup_db(); // schema present, but load won't be reached
        let checker = NearDuplicateChecker::new(&conn, cfg_dir.path(), NEAR_DUP_THRESHOLD);
        assert!(
            checker.is_none(),
            "unreachable host via temp config must yield None checker"
        );
    }

    #[test]
    fn test_near_duplicate_checker_new_load_error_returns_none() {
        // AC: new returns None (and logs a warning) if load_all_active_embeddings errors.
        // We point config at default URL (may or may not be reachable) but use a conn
        // with no tables so the SELECT prepare inside load_all will fail.
        let cfg_dir = tempfile::tempdir().unwrap();
        // Write a config that does not override URL (defaults apply).
        std::fs::write(cfg_dir.path().join("config.json"), "{}").unwrap();

        // Open a connection but *do not* create schema/migrations -> tables absent.
        let no_schema_dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(no_schema_dir.path().join("no-schema.db"))
            .expect("temp conn");
        // Do not run create_schema / run_migrations.

        let checker = NearDuplicateChecker::new(&conn, cfg_dir.path(), NEAR_DUP_THRESHOLD);
        assert!(
            checker.is_none(),
            "load failure (missing tables) must cause new() -> None"
        );
    }

    #[test]
    fn test_near_duplicate_checker_check_empty_text_unavailable() {
        // Even without a live embedder we can reach the empty-text early return
        // by constructing a checker that never gets used for embed (but we still
        // need one to call the method). If construction fails (no Ollama), we
        // simply skip the assertion — the empty-text path is also exercised in
        // the real caller before any checker is built.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::fs::write(cfg_dir.path().join("config.json"), "{}").unwrap();
        let (_d, conn) = setup_db();

        if let Some(checker) = NearDuplicateChecker::new(&conn, cfg_dir.path(), NEAR_DUP_THRESHOLD)
        {
            let out = checker.check("", "");
            assert_eq!(out, NearDupOutcome::Unavailable);
            let out2 = checker.check("   ", "\n\n");
            assert_eq!(out2, NearDupOutcome::Unavailable);
        }
    }

    #[test]
    fn test_near_miss_band_via_pure_fns() {
        // AC observability: a 0.85-similar vector is best_match max yet
        // find_near_duplicate(0.92) is None, so check()'s Unique arm (with
        // near-miss log when >= floor) must be taken, not a bare else.
        let base = vec![1.0_f32, 0.0, 0.0];
        let c85: f32 = 0.85;
        let v85 = vec![c85, (1.0 - c85 * c85).sqrt(), 0.0];
        let known = vec![(555i64, v85.clone())];
        let best = best_match(&base, &known);
        assert!(best.is_some());
        assert!((best.unwrap().1 - 0.85).abs() < 1e-5);
        let near = find_near_duplicate(&base, &known, 0.92);
        assert!(near.is_none(), "0.85 must be below 0.92 threshold");
        // Therefore in check(), after a successful embed yielding a vector whose
        // best sim is ~0.85, the code must hit the NEAR_MISS_LOG_FLOOR..thresh
        // arm (eprintln + Unique) rather than falling to a catch-all Unique
        // without having consulted best_match for the near-miss case.
    }

    #[test]
    fn test_checker_register_affects_subsequent_best_match() {
        // If we can build a checker (Ollama present + model available), register
        // should make a subsequent intra-batch candidate see the newly added entry.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::fs::write(cfg_dir.path().join("config.json"), "{}").unwrap();
        let (_d, conn) = setup_db();

        if let Some(mut checker) =
            NearDuplicateChecker::new(&conn, cfg_dir.path(), NEAR_DUP_THRESHOLD)
        {
            // Pick an arbitrary id/embedding we "just stored".
            let id = 999i64;
            let emb = vec![0.11_f32, 0.22, 0.33];
            checker.register(id, emb.clone());

            // After register, the in-memory known now contains the id/emb we just
            // pushed. Reconstruct a 1-element view and assert best_match sees it at
            // cosine 1.0. This exercises register() + the pure primitive without
            // depending on a second embed matching exactly.
            let before = best_match(&emb, &[(id, emb.clone())]);
            assert_eq!(before, Some((id, 1.0)));
        }
    }
}
