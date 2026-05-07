//! Composite backend that merges results from multiple backends.
//!
//! Runs all backends, deduplicates by learning ID (keeping the highest
//! relevance score), and returns the merged results sorted by score.
//! Uses stable sort so equal-score results preserve their backend insertion
//! order (which reflects SQL ordering).

use std::collections::HashMap;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::models::Learning;

use super::{RetrievalBackend, RetrievalQuery, ScoredLearning};

/// A composite backend that fans out queries to multiple backends and
/// merges their results.
pub struct CompositeBackend {
    backends: Vec<Box<dyn RetrievalBackend>>,
}

impl CompositeBackend {
    /// Creates a composite with the default built-in backends: FTS5 + Patterns + Vector.
    pub fn default_backends() -> Self {
        Self {
            backends: vec![
                Box::new(super::Fts5Backend),
                Box::new(super::PatternsBackend),
                Box::new(super::VectorBackend::default()),
            ],
        }
    }

    /// Creates a composite with the default backends but a config-aware
    /// `VectorBackend`.
    ///
    /// `strict == true` configures the vector backend to propagate Ollama
    /// failures as [`crate::TaskMgrError::OllamaUnreachable`] (see
    /// [`super::VectorBackend::with_strict_mode`]). The CLI passes
    /// `!allow_degraded` here so `recall --query` hard-fails by default and
    /// `--allow-degraded` opts out.
    pub fn with_ollama_config(ollama_url: &str, model: &str, strict: bool) -> Self {
        Self {
            backends: vec![
                Box::new(super::Fts5Backend),
                Box::new(super::PatternsBackend),
                Box::new(super::VectorBackend::new(ollama_url, model).with_strict_mode(strict)),
            ],
        }
    }

    /// Creates a composite with custom backends.
    pub fn new(backends: Vec<Box<dyn RetrievalBackend>>) -> Self {
        Self { backends }
    }
}

impl CompositeBackend {
    /// Fans out to all backends, deduplicates by ID (keeping max relevance score,
    /// concatenating match reasons), and sorts descending by relevance.
    ///
    /// `fetch_for_rerank` selects `retrieve_for_rerank` vs `retrieve` on each backend.
    /// `truncate_to_limit` truncates the result to `query.limit` when true.
    fn merge_from_backends(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
        fetch_for_rerank: bool,
        truncate_to_limit: bool,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        let mut merged: Vec<ScoredLearning> = Vec::new();
        let mut index: HashMap<i64, usize> = HashMap::new();

        for backend in &self.backends {
            let results = if fetch_for_rerank {
                backend.retrieve_for_rerank(conn, query)?
            } else {
                backend.retrieve(conn, query)?
            };

            for scored in results {
                let id = scored.learning.id.unwrap_or(-1);
                if id < 0 {
                    continue;
                }

                if let Some(&idx) = index.get(&id) {
                    let existing = &mut merged[idx];
                    if scored.relevance_score > existing.relevance_score {
                        existing.relevance_score = scored.relevance_score;
                    }
                    if let Some(ref new_reason) = scored.match_reason {
                        match existing.match_reason {
                            Some(ref mut existing_reason) => {
                                existing_reason.push_str("; ");
                                existing_reason.push_str(new_reason);
                            }
                            None => {
                                existing.match_reason = Some(new_reason.clone());
                            }
                        }
                    }
                } else {
                    index.insert(id, merged.len());
                    merged.push(scored);
                }
            }
        }

        // Stable sort: equal scores preserve insertion order (= backend SQL ordering)
        merged.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if truncate_to_limit {
            merged.truncate(query.limit);
        }

        Ok(merged)
    }
}

impl RetrievalBackend for CompositeBackend {
    fn name(&self) -> &str {
        "composite"
    }

    fn retrieve(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        self.merge_from_backends(conn, query, false, true)
    }

    fn retrieve_for_rerank(
        &self,
        conn: &Connection,
        query: &RetrievalQuery,
    ) -> TaskMgrResult<Vec<ScoredLearning>> {
        // No truncation — the caller (recall pipeline) truncates after reranking.
        self.merge_from_backends(conn, query, true, false)
    }

    fn index(&self, conn: &Connection, learning: &Learning) -> TaskMgrResult<()> {
        for backend in &self.backends {
            backend.index(conn, learning)?;
        }
        Ok(())
    }

    fn remove(&self, conn: &Connection, learning_id: i64) -> TaskMgrResult<()> {
        for backend in &self.backends {
            backend.remove(conn, learning_id)?;
        }
        Ok(())
    }
}
