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
        let health_agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(3))
            .timeout_read(std::time::Duration::from_secs(3))
            .build();
        let embed_agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(3))
            .timeout_read(std::time::Duration::from_secs(30))
            .build();
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

        let resp = match self.health_agent.get(&url).call() {
            Ok(r) => r,
            Err(e) => return Err(format!("Ollama not reachable at {}: {e}", self.base_url)),
        };

        let body: serde_json::Value = resp
            .into_json()
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

        let resp = self
            .embed_agent
            .post(&url)
            .send_json(&payload)
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("Ollama embed request failed: {e}"),
                )
            })?;

        let body: serde_json::Value = resp.into_json().map_err(|e| {
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
}
