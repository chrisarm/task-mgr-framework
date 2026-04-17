//! LearningWriter: single chokepoint for creating learnings + scheduling embeddings.

use std::path::Path;

use rusqlite::Connection;

use super::create::record_learning;
use super::types::{RecordLearningParams, RecordLearningResult};
use crate::TaskMgrResult;
use crate::learnings::embeddings::try_embed_learnings_batch;

struct PendingEmbed {
    learning_id: i64,
    title: String,
    content: String,
}

/// Facade that records learnings and defers Ollama embedding calls until [`flush`](Self::flush).
///
/// Every production code path that creates learnings should go through this struct.
/// Call [`record`](Self::record) one or more times, then [`flush`](Self::flush) after
/// any enclosing transaction has committed. Embedding failures are swallowed —
/// `curate embed` can backfill later.
pub struct LearningWriter<'a> {
    db_dir: Option<&'a Path>,
    pending: Vec<PendingEmbed>,
}

impl<'a> LearningWriter<'a> {
    pub fn new(db_dir: Option<&'a Path>) -> Self {
        Self {
            db_dir,
            pending: Vec::new(),
        }
    }

    /// Record a learning and queue it for embedding if `db_dir` is set.
    ///
    /// Accepts `&Connection` or `&Transaction` (`Transaction` derefs to `Connection`).
    /// When inside a `conn.transaction()` block, pass `&tx` — not `&conn` — because
    /// `conn` is mutably borrowed.
    pub fn record(
        &mut self,
        conn: &Connection,
        params: RecordLearningParams,
    ) -> TaskMgrResult<RecordLearningResult> {
        let title = params.title.clone();
        let content = params.content.clone();
        let result = record_learning(conn, params)?;
        if self.db_dir.is_some() {
            self.pending.push(PendingEmbed {
                learning_id: result.learning_id,
                title,
                content,
            });
        }
        Ok(result)
    }

    /// Queue an already-recorded learning for embedding (used by callers like `merge_cluster`
    /// that call `record_learning` themselves).
    pub fn push_existing(&mut self, learning_id: i64, title: String, content: String) {
        if self.db_dir.is_some() {
            self.pending.push(PendingEmbed {
                learning_id,
                title,
                content,
            });
        }
    }

    /// Consume the writer and embed all pending learnings via Ollama.
    ///
    /// Consumes `self` to prevent reuse. Call AFTER any active transaction has committed.
    /// Ollama errors are swallowed; the returned count is advisory — callers can ignore it.
    pub fn flush(mut self, conn: &Connection) -> usize {
        let Some(dir) = self.db_dir else { return 0 };
        if self.pending.is_empty() {
            return 0;
        }
        let items: Vec<(i64, String, String)> = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|p| (p.learning_id, p.title, p.content))
            .collect();
        try_embed_learnings_batch(conn, dir, &items)
    }
}

impl Drop for LearningWriter<'_> {
    fn drop(&mut self) {
        let n = self.pending.len();
        if n > 0 {
            eprintln!(
                "Warning: LearningWriter dropped with {n} un-flushed pending embedding(s). Call .flush() before dropping."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learnings::test_helpers::setup_db;
    use crate::models::{Confidence, LearningOutcome};

    fn make_params(title: &str) -> RecordLearningParams {
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: title.to_string(),
            content: "test content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        }
    }

    #[test]
    fn test_record_without_db_dir_skips_pending() {
        let (_dir, conn) = setup_db();
        let mut writer = LearningWriter::new(None);
        let result = writer
            .record(&conn, make_params("no-embed learning"))
            .unwrap();
        assert!(result.learning_id > 0);
        assert_eq!(writer.flush(&conn), 0);
    }

    #[test]
    fn test_record_with_db_dir_queues_pending() {
        let (dir, conn) = setup_db();
        let mut writer = LearningWriter::new(Some(dir.path()));
        writer.record(&conn, make_params("embed learning")).unwrap();
        // flush attempts embedding; result depends on Ollama availability
        let count = writer.flush(&conn);
        assert!(count <= 1);
    }

    #[test]
    fn test_flush_empty_returns_zero() {
        let (dir, conn) = setup_db();
        let writer = LearningWriter::new(Some(dir.path()));
        assert_eq!(writer.flush(&conn), 0);
    }

    #[test]
    fn test_record_inside_transaction() {
        let (dir, conn) = setup_db();
        let mut writer = LearningWriter::new(Some(dir.path()));
        {
            let tx = conn.unchecked_transaction().unwrap();
            let result = writer.record(&tx, make_params("tx learning")).unwrap();
            assert!(result.learning_id > 0);
            tx.commit().unwrap();
        }
        // flush attempts embedding; result depends on Ollama availability
        let _count = writer.flush(&conn);
    }

    #[test]
    fn test_drop_warning_does_not_panic() {
        let (dir, conn) = setup_db();
        let mut writer = LearningWriter::new(Some(dir.path()));
        writer.record(&conn, make_params("will drop")).unwrap();
        // Dropping with pending items must NOT panic
        drop(writer);
    }

    #[test]
    fn test_push_existing_queues_when_db_dir_set() {
        let (dir, conn) = setup_db();
        let mut writer = LearningWriter::new(Some(dir.path()));
        writer.push_existing(42, "title".to_string(), "content".to_string());
        // Ollama unavailable — returns 0 gracefully
        assert_eq!(writer.flush(&conn), 0);
    }

    #[test]
    fn test_push_existing_skips_when_no_db_dir() {
        let (_dir, conn) = setup_db();
        let mut writer = LearningWriter::new(None);
        writer.push_existing(42, "title".to_string(), "content".to_string());
        assert_eq!(writer.flush(&conn), 0);
    }

    #[test]
    fn test_record_learning_error_does_not_leak_pending() {
        let (dir, conn) = setup_db();
        let mut writer = LearningWriter::new(Some(dir.path()));
        let mut bad_params = make_params("will fail");
        bad_params.task_id = Some("nonexistent-task-id".to_string());
        // record_learning should fail due to FK violation
        let err = writer.record(&conn, bad_params);
        assert!(err.is_err());
        // pending must be empty — no phantom entry
        assert_eq!(writer.flush(&conn), 0);
    }
}
