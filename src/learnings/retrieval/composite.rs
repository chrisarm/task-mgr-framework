//! Composite backend that merges results from multiple backends.
//!
//! Runs all backends, deduplicates by learning ID (keeping the highest
//! relevance score), and returns the merged results sorted by score.
//! Uses stable sort so equal-score results preserve their backend insertion
//! order (which reflects SQL ordering).

use rusqlite::Connection;

use crate::models::Learning;
use crate::TaskMgrResult;

use super::{RetrievalBackend, RetrievalQuery, ScoredLearning};

/// A composite backend that fans out queries to multiple backends and
/// merges their results.
pub struct CompositeBackend {
    backends: Vec<Box<dyn RetrievalBackend>>,
}

impl CompositeBackend {
    /// Creates a composite with the default built-in backends: FTS5 + Patterns.
    pub fn default_backends() -> Self {
        Self {
            backends: vec![
                Box::new(super::Fts5Backend),
                Box::new(super::PatternsBackend),
            ],
        }
    }

    /// Creates a composite with custom backends.
    pub fn new(backends: Vec<Box<dyn RetrievalBackend>>) -> Self {
        Self { backends }
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
        // Collect results from all backends, preserving insertion order.
        let mut merged: Vec<ScoredLearning> = Vec::new();

        for backend in &self.backends {
            let results = backend.retrieve(conn, query)?;

            for scored in results {
                let id = scored.learning.id.unwrap_or(-1);
                if id < 0 {
                    continue;
                }

                // Dedup: check if already present (O(n) scan, fine for small result sets)
                if let Some(existing) = merged.iter_mut().find(|s| s.learning.id == Some(id)) {
                    // Keep the higher relevance score
                    if scored.relevance_score > existing.relevance_score {
                        existing.relevance_score = scored.relevance_score;
                    }
                    // Concatenate match reasons
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

        // Truncate to limit
        merged.truncate(query.limit);

        Ok(merged)
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
