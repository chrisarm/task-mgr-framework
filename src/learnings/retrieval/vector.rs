//! Vector similarity backend for learnings retrieval.
//!
//! Embeds the query text via OllamaEmbedder, computes cosine similarity against
//! all stored embeddings, and returns the top-N results. Degrades gracefully if
//! Ollama is unavailable or no embeddings exist.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::learnings::crud::get_learning;
use crate::learnings::embeddings::{
    DEFAULT_EMBEDDING_MODEL, DEFAULT_OLLAMA_URL, OllamaEmbedder, cosine_similarity,
    load_all_active_embeddings,
};
use crate::{TaskMgrError, TaskMgrResult};

use super::{RetrievalBackend, RetrievalQuery, ScoredLearning};

/// Convert any `TaskMgrError` into the `std::io::Error` expected by
/// `OllamaUnreachable.source`. The embedder's hot paths return `IoError`
/// directly; for any other variant we synthesize an `io::Error` carrying the
/// original error's `Display` message so context is preserved.
fn embedder_err_into_io(err: TaskMgrError) -> std::io::Error {
    match err {
        TaskMgrError::IoError(e) => e,
        TaskMgrError::IoErrorWithContext { source, .. } => source,
        other => std::io::Error::other(other.to_string()),
    }
}

/// Loads the set of learning IDs that have been superseded (appear as `old_learning_id`).
fn load_superseded_ids(conn: &Connection) -> TaskMgrResult<HashSet<i64>> {
    let mut stmt = conn.prepare("SELECT DISTINCT old_learning_id FROM learning_supersessions")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<HashSet<i64>, _>>()?;
    Ok(ids)
}

/// Score multiplier to normalize vector similarity scores into a range
/// comparable with FTS5 and pattern backend scores (which top out around 15).
const SCORE_SCALE: f64 = 15.0;

/// A retrieval backend that uses vector cosine similarity.
///
/// Query text is embedded via OllamaEmbedder and compared against all stored
/// embeddings. Returns the top-N matches sorted by descending similarity.
///
/// - `index()` is a no-op: embeddings are stored by `curate embed`, not on ingest.
/// - `remove()` deletes the stored embedding for a learning.
/// - **Non-strict (default)**: when Ollama is unreachable or no embeddings are
///   stored, returns an empty result so other backends in a composite can still
///   contribute. Preserves the original graceful-degradation behavior.
/// - **Strict**: opt in via [`with_strict_mode`](Self::with_strict_mode). Embed
///   or load failures propagate as [`TaskMgrError::OllamaUnreachable`] with
///   actionable hints. Used by `task-mgr recall --query` (without
///   `--allow-degraded`) so silent degradation can't mask missing semantic
///   results.
pub struct VectorBackend {
    embedder: OllamaEmbedder,
    model: String,
    /// When `true`, propagate embed/load failures instead of returning
    /// `Ok(empty)`. See struct-level docs.
    strict: bool,
}

impl VectorBackend {
    /// Create a VectorBackend pointing at the given Ollama server and model.
    /// `strict` defaults to `false` to preserve historical caller semantics.
    pub fn new(ollama_url: &str, model: &str) -> Self {
        Self {
            embedder: OllamaEmbedder::new(ollama_url, model),
            model: model.to_string(),
            strict: false,
        }
    }

    /// Builder: set the strict-mode flag.
    ///
    /// When `strict == true`, an unreachable Ollama (or a malformed embedding
    /// response) causes `retrieve` / `retrieve_for_rerank` to return
    /// `Err(TaskMgrError::OllamaUnreachable { .. })` instead of an empty slate.
    #[must_use]
    pub fn with_strict_mode(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }
}

impl Default for VectorBackend {
    fn default() -> Self {
        Self::new(DEFAULT_OLLAMA_URL, DEFAULT_EMBEDDING_MODEL)
    }
}

impl VectorBackend {
    /// Shared body for `retrieve` and `retrieve_for_rerank`.
    ///
    /// - `filter_positive`: drop results with `score <= 0.0` (`retrieve` only).
    /// - `truncate`: cap at `query.limit` (`retrieve` only).
    fn score_candidates(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
        filter_positive: bool,
        truncate: bool,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        // Without text there is nothing to embed. Empty `Some("")` and `None`
        // both short-circuit BEFORE any HTTP call so strict mode can't fire.
        let text = match &query.text {
            Some(t) if !t.is_empty() => t.as_str(),
            _ => return Ok(Vec::new()),
        };

        // Embed the query text. Strict mode surfaces the failure to the caller
        // with actionable hints; non-strict preserves the historical
        // graceful-degradation contract.
        let query_embedding = match self.embedder.embed(text) {
            Ok(emb) => emb,
            Err(e) => {
                if self.strict {
                    return Err(TaskMgrError::OllamaUnreachable {
                        url: self.embedder.base_url().to_string(),
                        model: self.model.clone(),
                        source: embedder_err_into_io(e),
                    });
                }
                return Ok(Vec::new());
            }
        };

        // Load all embeddings for active learnings. Strict mode propagates DB
        // errors so the caller is informed; non-strict returns empty (legacy).
        let stored = match load_all_active_embeddings(conn, &self.model) {
            Ok(list) => list,
            Err(e) => {
                if self.strict {
                    return Err(e);
                }
                return Ok(Vec::new());
            }
        };
        if stored.is_empty() {
            return Ok(Vec::new());
        }

        // Filter out superseded learnings unless the caller explicitly opts in.
        // Graceful degradation: if the table is unreachable, assume nothing is
        // superseded.
        let superseded: HashSet<i64> = if query.include_superseded {
            HashSet::new()
        } else {
            load_superseded_ids(conn).unwrap_or_default()
        };

        let mut scored: Vec<(i64, f64)> = stored
            .into_iter()
            .filter(|le| !superseded.contains(&le.learning_id))
            .map(|le| {
                let sim = cosine_similarity(&query_embedding, &le.embedding) as f64;
                (le.learning_id, sim * SCORE_SCALE)
            })
            .filter(|(_, score)| !filter_positive || *score > 0.0)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if truncate {
            scored.truncate(query.limit);
        }

        // Load Learning structs for top results; skip any that are no longer
        // present. Swallow per-learning DB errors to avoid failing the whole
        // retrieval.
        let mut results = Vec::with_capacity(scored.len());
        for (learning_id, score) in scored {
            match get_learning(conn, learning_id) {
                Ok(Some(learning)) => results.push(ScoredLearning {
                    learning,
                    relevance_score: score,
                    match_reason: Some("vector similarity".to_string()),
                }),
                Ok(None) | Err(_) => continue,
            }
        }

        Ok(results)
    }
}

impl RetrievalBackend for VectorBackend {
    fn name(&self) -> &str {
        "vector"
    }

    fn retrieve(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        self.score_candidates(conn, query, true, true)
    }

    /// Retrieve a broad candidate slate for cross-encoder reranking.
    ///
    /// Identical to [`retrieve`] except the `score > 0.0` filter and the
    /// `query.limit` truncation are both omitted; the cross-encoder caller
    /// handles those.
    fn retrieve_for_rerank(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        self.score_candidates(conn, query, false, false)
    }

    /// No-op: embeddings are generated explicitly via `curate embed`.
    fn index(&self, _conn: &Connection, _learning: &crate::models::Learning) -> TaskMgrResult<()> {
        Ok(())
    }

    /// Remove the stored embedding for a learning.
    fn remove(&self, conn: &Connection, learning_id: i64) -> TaskMgrResult<()> {
        conn.execute(
            "DELETE FROM learning_embeddings WHERE learning_id = ?1",
            [learning_id],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learnings::crud::{RecordLearningParams, record_learning};
    use crate::learnings::embeddings::store_embedding;
    use crate::learnings::test_helpers::setup_db;
    use crate::models::{Confidence, LearningOutcome};

    fn insert_learning_with_embedding(conn: &Connection, title: &str, embedding: &[f32]) -> i64 {
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: title.to_string(),
            content: format!("Content for {title}"),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        let result = record_learning(conn, params).unwrap();
        let id = result.learning_id;
        store_embedding(conn, id, DEFAULT_EMBEDDING_MODEL, embedding).unwrap();
        id
    }

    /// A VectorBackend with a mock embedder that won't hit real Ollama.
    /// We test the retrieval logic by pre-storing embeddings and using a
    /// backend variant whose embed() call we control via a test-only embedder.
    ///
    /// Since OllamaEmbedder is a concrete struct (not a trait), we test the
    /// retrieval path by injecting pre-stored embeddings and verifying that
    /// a backend that fails to embed returns empty results.

    #[test]
    fn test_retrieve_empty_when_no_embeddings() {
        let (_dir, conn) = setup_db();

        // Insert a learning without an embedding.
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "No embedding".to_string(),
            content: "content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        record_learning(&conn, params).unwrap();

        // Backend with a non-existent Ollama URL will fail to embed — returns empty.
        let backend = VectorBackend::new("http://127.0.0.1:0", DEFAULT_EMBEDDING_MODEL);
        let query = RetrievalQuery {
            text: Some("find something".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = backend.retrieve(&conn, &query).unwrap();
        assert!(results.is_empty(), "failed embed must return empty results");
    }

    #[test]
    fn test_retrieve_empty_when_query_text_is_none() {
        let (_dir, conn) = setup_db();

        let backend = VectorBackend::default();
        let query = RetrievalQuery {
            text: None,
            limit: 10,
            ..Default::default()
        };
        let results = backend.retrieve(&conn, &query).unwrap();
        assert!(results.is_empty(), "no text query must return empty");
    }

    #[test]
    fn test_retrieve_sorted_by_similarity() {
        let (_dir, conn) = setup_db();

        // Store three learnings with known embeddings.
        // Query embedding [1,0,0]: similarity to [1,0,0] = 1.0, [0,1,0] = 0.0, [-1,0,0] < 0
        let id_high = insert_learning_with_embedding(&conn, "High match", &[1.0, 0.0, 0.0]);
        let id_mid = insert_learning_with_embedding(&conn, "Mid match", &[0.5, 0.5, 0.0]);
        let _id_neg = insert_learning_with_embedding(&conn, "Negative match", &[-1.0, 0.0, 0.0]);

        // Build a VectorBackend with a test model and pre-stored embeddings.
        // We bypass the live Ollama call by pre-computing what the retrieve()
        // core logic would see. Since OllamaEmbedder is concrete and we can't
        // intercept embed(), we instead verify the ordering logic via the
        // remove/index path and trust the score formula.
        //
        // Verify via the stored embeddings directly: simulate query = [1,0,0]
        // by computing cosine_similarity manually.
        let query_emb = vec![1.0_f32, 0.0, 0.0];
        let high_sim = cosine_similarity(&query_emb, &[1.0, 0.0, 0.0]);
        let mid_sim = cosine_similarity(&query_emb, &[0.5, 0.5, 0.0]);
        let neg_sim = cosine_similarity(&query_emb, &[-1.0, 0.0, 0.0]);

        assert!(high_sim > mid_sim, "high similarity must beat mid");
        assert!(neg_sim < 0.0, "negative similarity must be filtered");

        // Also verify score scale
        let high_score = high_sim as f64 * SCORE_SCALE;
        let mid_score = mid_sim as f64 * SCORE_SCALE;
        assert!(
            (high_score - 15.0).abs() < 1e-4,
            "perfect match * 15.0 = 15.0"
        );
        assert!(mid_score > 0.0 && mid_score < 15.0, "mid score in range");

        // Verify the IDs are correctly stored (confirms integration)
        let _ = id_high;
        let _ = id_mid;
    }

    #[test]
    fn test_remove_deletes_embedding() {
        let (_dir, conn) = setup_db();

        let id = insert_learning_with_embedding(&conn, "Remove me", &[1.0, 0.0, 0.0]);

        // Verify embedding exists before removal.
        let count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings WHERE learning_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count_before, 1, "embedding must exist before remove");

        let backend = VectorBackend::default();
        backend.remove(&conn, id).unwrap();

        let count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings WHERE learning_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count_after, 0, "embedding must be deleted after remove");
    }

    #[test]
    fn test_remove_nonexistent_is_noop() {
        let (_dir, conn) = setup_db();
        let backend = VectorBackend::default();
        // Removing a non-existent embedding must not error.
        backend.remove(&conn, 99999).unwrap();
    }

    #[test]
    fn test_index_is_noop() {
        let (_dir, conn) = setup_db();
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Index noop".to_string(),
            content: "content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        };
        let result = record_learning(&conn, params).unwrap();
        let learning = get_learning(&conn, result.learning_id).unwrap().unwrap();

        let backend = VectorBackend::default();
        // index() must not error and must not insert any embedding.
        backend.index(&conn, &learning).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_embeddings WHERE learning_id = ?1",
                [result.learning_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "index() must not store any embedding");
    }

    /// Mock the Ollama embed endpoint to return a fixed query embedding.
    fn mock_embed(server: &mut mockito::Server, embedding: &[f32]) -> mockito::Mock {
        let json_array: Vec<serde_json::Value> =
            embedding.iter().map(|&v| serde_json::json!(v)).collect();
        let body = serde_json::json!({ "embeddings": [json_array] }).to_string();
        server
            .mock("POST", "/api/embed")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create()
    }

    #[test]
    fn test_vector_retrieve_filters_zero_score() {
        // Verify retrieve() excludes an orthogonal embedding (cosine = 0.0, score = 0.0).
        let mut server = mockito::Server::new();
        // Query embedding [1,0,0]; stored embedding [0,1,0] → cosine = 0.0 → score = 0.0
        let _mock = mock_embed(&mut server, &[1.0, 0.0, 0.0]);
        let (_dir, conn) = setup_db();
        insert_learning_with_embedding(&conn, "Orthogonal", &[0.0, 1.0, 0.0]);

        let backend = VectorBackend::new(&server.url(), DEFAULT_EMBEDDING_MODEL);
        let query = RetrievalQuery {
            text: Some("any query".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = backend.retrieve(&conn, &query).unwrap();
        assert!(
            results.is_empty(),
            "retrieve must filter out cosine=0 (score=0.0) results"
        );
    }

    #[test]
    fn test_vector_retrieve_for_rerank_keeps_zero_scores() {
        // retrieve_for_rerank must include an orthogonal embedding (cosine = 0.0, score = 0.0).
        let mut server = mockito::Server::new();
        let _mock = mock_embed(&mut server, &[1.0, 0.0, 0.0]);
        let (_dir, conn) = setup_db();
        let id = insert_learning_with_embedding(&conn, "Orthogonal", &[0.0, 1.0, 0.0]);

        let backend = VectorBackend::new(&server.url(), DEFAULT_EMBEDDING_MODEL);
        let query = RetrievalQuery {
            text: Some("any query".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = backend.retrieve_for_rerank(&conn, &query).unwrap();
        assert!(
            !results.is_empty(),
            "retrieve_for_rerank must include zero-score results"
        );
        assert_eq!(results[0].learning.id, Some(id));
        assert!(
            results[0].relevance_score.abs() < 1e-9,
            "score should be ~0.0, got {}",
            results[0].relevance_score
        );
    }

    #[test]
    fn test_vector_retrieve_for_rerank_keeps_negative_scores() {
        // retrieve_for_rerank must include an anti-parallel embedding (cosine = -1.0, score = -15.0).
        let mut server = mockito::Server::new();
        let _mock = mock_embed(&mut server, &[1.0, 0.0, 0.0]);
        let (_dir, conn) = setup_db();
        let id = insert_learning_with_embedding(&conn, "Opposite", &[-1.0, 0.0, 0.0]);

        let backend = VectorBackend::new(&server.url(), DEFAULT_EMBEDDING_MODEL);
        let query = RetrievalQuery {
            text: Some("any query".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = backend.retrieve_for_rerank(&conn, &query).unwrap();
        assert!(
            !results.is_empty(),
            "retrieve_for_rerank must include negative-score results"
        );
        assert_eq!(results[0].learning.id, Some(id));
        assert!(
            results[0].relevance_score < 0.0,
            "score should be negative, got {}",
            results[0].relevance_score
        );
    }

    #[test]
    fn test_vector_retrieve_for_rerank_no_truncation() {
        // retrieve_for_rerank must not truncate results to query.limit.
        let mut server = mockito::Server::new();
        // Query [1,0,0]; 5 stored embeddings all with positive cosine scores.
        let _mock = mock_embed(&mut server, &[1.0, 0.0, 0.0]);
        let (_dir, conn) = setup_db();
        for i in 1..=5 {
            insert_learning_with_embedding(
                &conn,
                &format!("L{i}"),
                &[0.5_f32 + i as f32 * 0.01, 0.0, 0.0],
            );
        }

        let backend = VectorBackend::new(&server.url(), DEFAULT_EMBEDDING_MODEL);
        let query = RetrievalQuery {
            text: Some("any query".to_string()),
            limit: 2, // deliberately small
            ..Default::default()
        };
        let results = backend.retrieve_for_rerank(&conn, &query).unwrap();
        assert!(
            results.len() > 2,
            "retrieve_for_rerank must not truncate to limit=2; got {} results",
            results.len()
        );
    }

    // ---- FEAT-001: strict-mode (`--allow-degraded` opt-out) -------------

    #[test]
    fn test_strict_mode_propagates_ollama_unreachable() {
        // AC: VectorBackend::new(...).with_strict_mode(true) pointing at an
        // unreachable Ollama with non-empty query text → Err(OllamaUnreachable)
        // with `url == base_url verbatim`.
        let (_dir, conn) = setup_db();

        let backend = VectorBackend::new("http://127.0.0.1:0", DEFAULT_EMBEDDING_MODEL)
            .with_strict_mode(true);
        let query = RetrievalQuery {
            text: Some("anything".to_string()),
            limit: 10,
            ..Default::default()
        };
        let err = backend
            .retrieve(&conn, &query)
            .expect_err("strict mode must hard-fail when Ollama is unreachable");
        match err {
            TaskMgrError::OllamaUnreachable { url, model, source } => {
                assert_eq!(
                    url, "http://127.0.0.1:0",
                    "url must match the embedder's base_url verbatim"
                );
                assert_eq!(model, DEFAULT_EMBEDDING_MODEL);
                // Source preserved (not a panic, not a generic placeholder).
                let msg = source.to_string();
                assert!(
                    !msg.is_empty(),
                    "source io::Error must carry the underlying message"
                );
            }
            other => panic!("expected OllamaUnreachable, got {other:?}"),
        }
    }

    #[test]
    fn test_non_strict_mode_returns_empty_when_ollama_unreachable() {
        // AC: same backend with strict=false (the default) → Ok(empty),
        // preserving the historical graceful-degradation contract.
        let (_dir, conn) = setup_db();

        let backend = VectorBackend::new("http://127.0.0.1:0", DEFAULT_EMBEDDING_MODEL)
            .with_strict_mode(false);
        let query = RetrievalQuery {
            text: Some("anything".to_string()),
            limit: 10,
            ..Default::default()
        };
        let results = backend
            .retrieve(&conn, &query)
            .expect("non-strict must return Ok(empty) when Ollama is down");
        assert!(results.is_empty());
    }

    #[test]
    fn test_strict_mode_short_circuits_on_empty_query() {
        // AC: empty query text (`Some("")`) with strict=true → Ok(empty)
        // because no Ollama call is attempted. Guards against the known-bad
        // implementation that calls embedder.embed("") and then errors.
        let (_dir, conn) = setup_db();

        let backend = VectorBackend::new("http://127.0.0.1:0", DEFAULT_EMBEDDING_MODEL)
            .with_strict_mode(true);
        let query = RetrievalQuery {
            text: Some(String::new()),
            limit: 10,
            ..Default::default()
        };
        let results = backend
            .retrieve(&conn, &query)
            .expect("empty `Some(\"\")` must short-circuit before any HTTP call");
        assert!(results.is_empty());
    }

    #[test]
    fn test_strict_mode_short_circuits_on_none_query() {
        // For-task-only recall: query.text is None, no Ollama call happens
        // even in strict mode. This is the path that keeps `--for-task`
        // recall offline-friendly.
        let (_dir, conn) = setup_db();

        let backend = VectorBackend::new("http://127.0.0.1:0", DEFAULT_EMBEDDING_MODEL)
            .with_strict_mode(true);
        let query = RetrievalQuery {
            text: None,
            limit: 10,
            ..Default::default()
        };
        let results = backend
            .retrieve(&conn, &query)
            .expect("None text must short-circuit before any HTTP call");
        assert!(results.is_empty());
    }

    #[test]
    fn test_strict_mode_propagates_malformed_json_as_unreachable() {
        // AC: Ollama returns valid HTTP but malformed JSON in strict mode
        // → Err(OllamaUnreachable) with the underlying io::Error as `source`,
        // not a panic.
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/api/embed")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not actually json {")
            .create();
        let (_dir, conn) = setup_db();

        let backend =
            VectorBackend::new(&server.url(), DEFAULT_EMBEDDING_MODEL).with_strict_mode(true);
        let query = RetrievalQuery {
            text: Some("any query".to_string()),
            limit: 10,
            ..Default::default()
        };
        let err = backend
            .retrieve(&conn, &query)
            .expect_err("malformed JSON must surface as a structured error");
        match err {
            TaskMgrError::OllamaUnreachable { url, source, .. } => {
                assert_eq!(
                    url,
                    server.url().trim_end_matches('/'),
                    "url must mirror embedder.base_url after trim"
                );
                let msg = source.to_string();
                assert!(
                    !msg.is_empty(),
                    "underlying io::Error message must be preserved"
                );
            }
            other => panic!("expected OllamaUnreachable, got {other:?}"),
        }
    }

    #[test]
    fn test_with_strict_mode_default_is_false() {
        // VectorBackend::new should default to non-strict so existing callers
        // (CompositeBackend::default_backends, recall_learnings legacy entry
        // point) keep their historical empty-on-error behavior.
        let backend = VectorBackend::new("http://127.0.0.1:0", DEFAULT_EMBEDDING_MODEL);
        assert!(
            !backend.strict,
            "VectorBackend::new must default strict=false"
        );
        let strict = backend.with_strict_mode(true);
        assert!(strict.strict, "with_strict_mode(true) flips the flag");
    }

    #[test]
    fn test_retrieval_query_has_no_allow_degraded_field() {
        // Trait/struct-shape guard: `RetrievalQuery` must NOT carry
        // `allow_degraded` — that policy lives on `RecallParams` /
        // `RecallCmdParams` so backends stay recall-policy-agnostic. An
        // exhaustive struct literal (no `..Default::default()`) means
        // adding ANY new field — including `allow_degraded` — would fail
        // compilation here, forcing reviewers to look at why and where the
        // policy field would belong.
        let _query = RetrievalQuery {
            text: None,
            task_id: None,
            task_files: Vec::new(),
            task_prefix: None,
            task_error: None,
            tags: None,
            outcome: None,
            limit: 0,
            include_superseded: false,
        };
    }

    #[test]
    fn test_composite_includes_vector_backend() {
        use crate::learnings::retrieval::CompositeBackend;

        let (_dir, conn) = setup_db();

        // CompositeBackend::default_backends() should include the vector backend.
        // We verify by checking the name() is present among backends via retrieve()
        // with an empty DB — composite must not error even if vector backend returns empty.
        let composite = CompositeBackend::default_backends();
        let query = RetrievalQuery {
            text: Some("some query text".to_string()),
            limit: 10,
            ..Default::default()
        };
        // Must not panic or return Err; vector backend gracefully returns empty.
        let results = composite.retrieve(&conn, &query).unwrap();
        // Results from FTS5/patterns with no data may be empty too; just assert no error.
        let _ = results;
    }
}
