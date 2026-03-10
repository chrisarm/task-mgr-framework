//! Sibling PRD section builder for milestone tasks in batch mode.
//!
//! When the loop runs in batch mode and the current task is a MILESTONE,
//! this module injects context about remaining tasks in sibling PRDs.
//! This gives the milestone agent visibility into cross-PRD impacts so it
//! can update sibling tasks that reference files changed by this PRD.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::commands::init::parse::{PrdFile, PrdUserStory};
use crate::db::prefix::prefix_and_col;

/// Maximum number of file-relevant tasks to show per sibling PRD.
const MAX_RELEVANT: usize = 5;
/// Maximum number of non-relevant tasks to show (one-liners) per sibling PRD.
const MAX_OTHER: usize = 5;
/// Maximum characters for description snippets.
const DESC_SNIPPET_LEN: usize = 100;

/// Intermediate representation of a parsed sibling PRD's remaining tasks.
struct SiblingPrdSummary {
    filename: String,
    prd_path: PathBuf,
    total_tasks: usize,
    remaining_tasks: usize,
    /// Tasks whose `touches_files` overlap with completed task files, sorted by overlap desc.
    relevant: Vec<(PrdUserStory, usize)>,
    /// Remaining tasks with no file overlap.
    other: Vec<PrdUserStory>,
}

/// Build a prompt section showing relevant sibling PRD tasks for milestone review.
///
/// Returns an empty string (auto-skipped by `try_fit_section`) when:
/// - The task is not a MILESTONE
/// - There are no sibling PRDs (single-PRD mode)
/// - No sibling PRDs have remaining tasks
pub(crate) fn build_sibling_prd_section(
    conn: &Connection,
    task_id: &str,
    task_prefix: Option<&str>,
    sibling_prd_paths: &[PathBuf],
) -> String {
    // Gate: only milestone tasks
    if !task_id.contains("MILESTONE") {
        return String::new();
    }

    // Gate: only batch mode
    if sibling_prd_paths.is_empty() {
        return String::new();
    }

    let completed_files = get_completed_task_files(conn, task_prefix);

    let summaries: Vec<SiblingPrdSummary> = sibling_prd_paths
        .iter()
        .filter_map(|path| read_sibling_summary(path, &completed_files))
        .filter(|s| s.remaining_tasks > 0)
        .collect();

    if summaries.is_empty() {
        return String::new();
    }

    format_sibling_section(&summaries, &completed_files)
}

/// Query distinct file paths from tasks marked as done, optionally scoped by prefix.
fn get_completed_task_files(conn: &Connection, task_prefix: Option<&str>) -> HashSet<String> {
    let (prefix_clause, prefix_param) = prefix_and_col("t.id", task_prefix);

    let sql = format!(
        "SELECT DISTINCT tf.file_path FROM task_files tf \
         INNER JOIN tasks t ON tf.task_id = t.id \
         WHERE t.status = 'done' {prefix_clause}"
    );

    let result: Result<Vec<String>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<String> = if let Some(ref param) = prefix_param {
            stmt.query_map([param], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?
        } else {
            stmt.query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?
        };
        Ok(rows)
    })();

    result.unwrap_or_default().into_iter().collect()
}

/// Parse a sibling PRD JSON file and partition its remaining tasks by file relevance.
fn read_sibling_summary(
    prd_path: &Path,
    completed_files: &HashSet<String>,
) -> Option<SiblingPrdSummary> {
    let content = std::fs::read_to_string(prd_path).ok()?;
    let prd: PrdFile = serde_json::from_str(&content).ok()?;

    let total_tasks = prd.user_stories.len();
    let remaining: Vec<PrdUserStory> = prd.user_stories.into_iter().filter(|s| !s.passes).collect();
    let remaining_count = remaining.len();

    if remaining_count == 0 {
        return None;
    }

    let mut relevant: Vec<(PrdUserStory, usize)> = Vec::new();
    let mut other: Vec<PrdUserStory> = Vec::new();

    for story in remaining {
        let overlap = score_relevance(&story.touches_files, completed_files);
        if overlap > 0 {
            relevant.push((story, overlap));
        } else {
            other.push(story);
        }
    }

    // Sort relevant by overlap count descending
    relevant.sort_by(|a, b| b.1.cmp(&a.1));

    let filename = prd_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| prd_path.display().to_string());

    Some(SiblingPrdSummary {
        filename,
        prd_path: prd_path.to_path_buf(),
        total_tasks,
        remaining_tasks: remaining_count,
        relevant,
        other,
    })
}

/// Count how many task files overlap with the completed files set.
fn score_relevance(task_files: &[String], completed_files: &HashSet<String>) -> usize {
    task_files
        .iter()
        .filter(|f| completed_files.contains(f.as_str()))
        .count()
}

/// Truncate a string to a character limit, appending "..." if truncated.
fn snippet(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

/// Format all sibling PRD summaries into a prompt section.
fn format_sibling_section(
    summaries: &[SiblingPrdSummary],
    completed_files: &HashSet<String>,
) -> String {
    let mut out = String::from("## Sibling PRD Tasks\n\n");
    out.push_str(
        "These are remaining tasks from other PRDs in this batch. \
         Review relevant tasks (file overlap with your completed work) \
         and update them if your implementation changed APIs, data structures, or file layouts.\n\n",
    );

    for summary in summaries {
        out.push_str(&format!(
            "### {} ({}/{} remaining)\n\n",
            summary.filename, summary.remaining_tasks, summary.total_tasks,
        ));

        // Relevant tasks (with file overlap)
        if !summary.relevant.is_empty() {
            out.push_str("**Relevant** (touches files changed by completed tasks):\n\n");
            for (story, overlap) in summary.relevant.iter().take(MAX_RELEVANT) {
                let desc = story
                    .description
                    .as_deref()
                    .map(|d| snippet(d, DESC_SNIPPET_LEN))
                    .unwrap_or_default();

                let overlapping_files: Vec<&str> = story
                    .touches_files
                    .iter()
                    .filter(|f| completed_files.contains(f.as_str()))
                    .map(|f| f.as_str())
                    .collect();

                out.push_str(&format!("- **{}**: {}\n", story.id, story.title));
                if !desc.is_empty() {
                    out.push_str(&format!("  {desc}\n"));
                }
                out.push_str(&format!(
                    "  _{overlap} overlapping file(s): {}_\n",
                    overlapping_files.join(", "),
                ));
            }
            if summary.relevant.len() > MAX_RELEVANT {
                out.push_str(&format!(
                    "  _...and {} more relevant tasks_\n",
                    summary.relevant.len() - MAX_RELEVANT,
                ));
            }
            out.push('\n');
        }

        // Other tasks (no file overlap, one-liners)
        if !summary.other.is_empty() {
            out.push_str("**Other remaining**:\n\n");
            for story in summary.other.iter().take(MAX_OTHER) {
                out.push_str(&format!("- {}: {}\n", story.id, story.title));
            }
            if summary.other.len() > MAX_OTHER {
                out.push_str(&format!(
                    "- _...and {} more_\n",
                    summary.other.len() - MAX_OTHER,
                ));
            }
            out.push('\n');
        }

        // Pointer to full PRD
        let shown = summary.relevant.len().min(MAX_RELEVANT) + summary.other.len().min(MAX_OTHER);
        if summary.remaining_tasks > shown {
            out.push_str(&format!(
                "_Read `{}` for the full list ({} total remaining)._\n\n",
                summary.prd_path.display(),
                summary.remaining_tasks,
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;
    use rusqlite::Connection;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_schema(&conn).unwrap();
        conn
    }

    fn insert_task(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, description, priority, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, format!("Task {id}"), "desc", 10, status],
        )
        .unwrap();
    }

    fn insert_task_file(conn: &Connection, task_id: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
            rusqlite::params![task_id, file_path],
        )
        .unwrap();
    }

    fn write_prd_file(stories: &[serde_json::Value]) -> NamedTempFile {
        let prd = serde_json::json!({
            "project": "test-project",
            "userStories": stories,
        });
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(prd.to_string().as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_non_milestone_returns_empty() {
        let conn = setup_db();
        let result = build_sibling_prd_section(&conn, "FEAT-001", None, &[PathBuf::from("a.json")]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_no_siblings_returns_empty() {
        let conn = setup_db();
        let result = build_sibling_prd_section(&conn, "MILESTONE-1", None, &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_sibling_all_tasks_pass_returns_empty() {
        let conn = setup_db();
        let prd = write_prd_file(&[serde_json::json!({
            "id": "FEAT-001",
            "title": "Done task",
            "priority": 10,
            "passes": true,
            "touchesFiles": ["src/main.rs"],
        })]);
        let result =
            build_sibling_prd_section(&conn, "MILESTONE-1", None, &[prd.path().to_path_buf()]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_sibling_with_relevant_tasks() {
        let conn = setup_db();
        insert_task(&conn, "prefix-FEAT-001", "done");
        insert_task_file(&conn, "prefix-FEAT-001", "src/api.rs");

        let prd = write_prd_file(&[
            serde_json::json!({
                "id": "SIB-001",
                "title": "Update API consumer",
                "description": "This task updates the API consumer to use the new endpoint",
                "priority": 10,
                "passes": false,
                "touchesFiles": ["src/api.rs", "src/client.rs"],
            }),
            serde_json::json!({
                "id": "SIB-002",
                "title": "Unrelated task",
                "priority": 20,
                "passes": false,
                "touchesFiles": ["src/unrelated.rs"],
            }),
        ]);

        let result =
            build_sibling_prd_section(&conn, "MILESTONE-1", None, &[prd.path().to_path_buf()]);

        assert!(result.contains("Sibling PRD Tasks"));
        assert!(result.contains("SIB-001"));
        assert!(result.contains("Update API consumer"));
        assert!(result.contains("1 overlapping file(s): src/api.rs"));
        assert!(result.contains("SIB-002"));
        assert!(result.contains("Unrelated task"));
    }

    #[test]
    fn test_sibling_with_prefix_scoping() {
        let conn = setup_db();
        insert_task(&conn, "abc-FEAT-001", "done");
        insert_task_file(&conn, "abc-FEAT-001", "src/shared.rs");
        // Task from different prefix — should be excluded when prefix is "abc"
        insert_task(&conn, "xyz-FEAT-001", "done");
        insert_task_file(&conn, "xyz-FEAT-001", "src/other.rs");

        let prd = write_prd_file(&[serde_json::json!({
            "id": "SIB-001",
            "title": "Uses shared",
            "priority": 10,
            "passes": false,
            "touchesFiles": ["src/shared.rs", "src/other.rs"],
        })]);

        let result = build_sibling_prd_section(
            &conn,
            "MILESTONE-1",
            Some("abc"),
            &[prd.path().to_path_buf()],
        );

        // Only "abc" prefix tasks are done, so only src/shared.rs is in completed_files
        assert!(result.contains("1 overlapping file(s): src/shared.rs"));
    }

    #[test]
    fn test_multiple_sibling_prds() {
        let conn = setup_db();
        insert_task(&conn, "FEAT-001", "done");
        insert_task_file(&conn, "FEAT-001", "src/shared.rs");

        let prd1 = write_prd_file(&[serde_json::json!({
            "id": "A-001",
            "title": "PRD-A task",
            "priority": 10,
            "passes": false,
            "touchesFiles": ["src/shared.rs"],
        })]);
        let prd2 = write_prd_file(&[serde_json::json!({
            "id": "B-001",
            "title": "PRD-B task",
            "priority": 10,
            "passes": false,
            "touchesFiles": ["src/other.rs"],
        })]);

        let result = build_sibling_prd_section(
            &conn,
            "MILESTONE-1",
            None,
            &[prd1.path().to_path_buf(), prd2.path().to_path_buf()],
        );

        assert!(result.contains("A-001"));
        assert!(result.contains("B-001"));
    }

    #[test]
    fn test_score_relevance() {
        let completed: HashSet<String> = ["a.rs", "b.rs", "c.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        assert_eq!(score_relevance(&[], &completed), 0);
        assert_eq!(
            score_relevance(&["a.rs".to_string(), "d.rs".to_string()], &completed),
            1
        );
        assert_eq!(
            score_relevance(&["a.rs".to_string(), "b.rs".to_string()], &completed),
            2
        );
    }

    #[test]
    fn test_snippet_within_limit() {
        assert_eq!(snippet("short", 10), "short");
    }

    #[test]
    fn test_snippet_exceeds_limit() {
        let result = snippet("a long description that exceeds the limit", 10);
        assert_eq!(result, "a long des...");
    }

    #[test]
    fn test_get_completed_task_files_empty_db() {
        let conn = setup_db();
        let files = get_completed_task_files(&conn, None);
        assert!(files.is_empty());
    }

    #[test]
    fn test_get_completed_task_files_filters_by_status() {
        let conn = setup_db();
        insert_task(&conn, "FEAT-001", "done");
        insert_task_file(&conn, "FEAT-001", "src/done.rs");
        insert_task(&conn, "FEAT-002", "in_progress");
        insert_task_file(&conn, "FEAT-002", "src/wip.rs");

        let files = get_completed_task_files(&conn, None);
        assert!(files.contains("src/done.rs"));
        assert!(!files.contains("src/wip.rs"));
    }

    #[test]
    fn test_count_limits_enforced() {
        let conn = setup_db();
        // Create many completed files so all sibling tasks are "relevant"
        for i in 0..12 {
            let task_id = format!("FEAT-{i:03}");
            let file_path = format!("src/file{i}.rs");
            insert_task(&conn, &task_id, "done");
            insert_task_file(&conn, &task_id, &file_path);
        }

        // Create sibling PRD with 12 relevant tasks
        let stories: Vec<serde_json::Value> = (0..12)
            .map(|i| {
                serde_json::json!({
                    "id": format!("SIB-{i:03}"),
                    "title": format!("Task {i}"),
                    "priority": i,
                    "passes": false,
                    "touchesFiles": [format!("src/file{i}.rs")],
                })
            })
            .collect();
        let prd = write_prd_file(&stories);

        let result =
            build_sibling_prd_section(&conn, "MILESTONE-1", None, &[prd.path().to_path_buf()]);

        // Should show MAX_RELEVANT (5) relevant tasks and indicate more
        assert!(result.contains("...and 7 more relevant tasks"));
        // No "Other remaining" section since all are relevant
        assert!(!result.contains("Other remaining"));
    }
}
