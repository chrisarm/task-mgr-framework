//! Deprecated synergy section: gutted to no-ops after parallel-task-execution
//! dropped the `synergyWith` / `batchWith` / `conflictsWith` relationship types
//! in favour of runtime file-overlap detection.
//!
//! The two functions are retained so `prompt.rs` call sites don't change, but
//! neither queries partners any more. Primary model resolution (task →
//! high→opus → prd/project/user defaults) still flows through
//! `model::resolve_task_model` so `prd_default` fallback and
//! `difficulty=high → opus` escalation behaviour is preserved.

use rusqlite::Connection;

use crate::loop_engine::model;

/// No-op: synergy context sections were removed along with synergy relationships.
///
/// Always returns an empty string. Parameters are kept to preserve the call-site
/// signature in `prompt.rs`.
pub(crate) fn build_synergy_section(
    _conn: &Connection,
    _task_id: &str,
    _run_id: Option<&str>,
) -> String {
    String::new()
}

/// Resolve the iteration's model and difficulty from the **primary task only**.
///
/// Prior to the parallel-task-execution refactor this walked `synergyWith`
/// partners to escalate the cluster's model and effort tier. Partner queries
/// have been removed: file-overlap conflict detection supersedes the synergy
/// relationship model. The primary task's own model/difficulty still flow
/// through `model::resolve_task_model` so `difficulty=high → opus` and the
/// `prd_default` / project / user default chain continue to work.
///
/// Signature preserved so call sites in `prompt.rs` do not change.
pub(crate) fn resolve_synergy_cluster(
    _conn: &Connection,
    _task_id: &str,
    primary_model: Option<&str>,
    primary_difficulty: Option<&str>,
    defaults: &model::ModelResolutionContext<'_>,
) -> (Option<String>, Option<String>) {
    let resolved_model = model::resolve_task_model(&model::ModelResolutionContext {
        task_model: primary_model,
        difficulty: primary_difficulty,
        ..*defaults
    })
    .filter(|m| !m.trim().is_empty());

    let resolved_difficulty = primary_difficulty
        .filter(|d| !d.trim().is_empty())
        .map(str::to_string);

    (resolved_model, resolved_difficulty)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::loop_engine::test_utils::setup_test_db;

    #[test]
    fn test_build_synergy_section_always_empty() {
        let (_temp_dir, conn) = setup_test_db();
        assert_eq!(build_synergy_section(&conn, "TASK-001", None), "");
        assert_eq!(build_synergy_section(&conn, "TASK-001", Some("run-1")), "");
    }

    #[test]
    fn test_resolve_synergy_cluster_primary_only_no_partner_query() {
        let (_temp_dir, conn) = setup_test_db();
        let defaults = model::ModelResolutionContext::default();

        let (m, d) = resolve_synergy_cluster(
            &conn,
            "TASK-001",
            Some(model::SONNET_MODEL),
            Some("medium"),
            &defaults,
        );
        assert_eq!(m.as_deref(), Some(model::SONNET_MODEL));
        assert_eq!(d.as_deref(), Some("medium"));
    }

    #[test]
    fn test_resolve_synergy_cluster_high_difficulty_forces_opus() {
        let (_temp_dir, conn) = setup_test_db();
        let defaults = model::ModelResolutionContext::default();

        let (m, d) = resolve_synergy_cluster(&conn, "TASK-001", None, Some("high"), &defaults);
        assert_eq!(
            m.as_deref(),
            Some(model::OPUS_MODEL),
            "difficulty=high must still escalate to opus via resolve_task_model"
        );
        assert_eq!(d.as_deref(), Some("high"));
    }

    #[test]
    fn test_resolve_synergy_cluster_prd_default_fallback() {
        let (_temp_dir, conn) = setup_test_db();
        let defaults = model::ModelResolutionContext {
            prd_default: Some(model::HAIKU_MODEL),
            ..Default::default()
        };

        let (m, _d) = resolve_synergy_cluster(&conn, "TASK-001", None, None, &defaults);
        assert_eq!(m.as_deref(), Some(model::HAIKU_MODEL));
    }

    #[test]
    fn test_resolve_synergy_cluster_empty_model_normalized_to_none() {
        let (_temp_dir, conn) = setup_test_db();
        let defaults = model::ModelResolutionContext::default();

        let (m, _d) = resolve_synergy_cluster(&conn, "TASK-001", Some(""), None, &defaults);
        assert!(
            m.is_none(),
            "empty-string model must never leak as Some(\"\")"
        );
    }
}
