//! PRD JSON export logic.
//!
//! This module handles exporting tasks and metadata back to PRD JSON format.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::TaskStatus;
use crate::TaskMgrResult;

/// Type alias for relationship maps loaded from database.
pub(crate) type RelationshipMaps = (
    HashMap<String, Vec<String>>, // depends_on
    HashMap<String, Vec<String>>, // synergy_with
    HashMap<String, Vec<String>>, // batch_with
    HashMap<String, Vec<String>>, // conflicts_with
);

/// JSON structure for a user story in the exported PRD.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedUserStory {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub priority: i32,
    pub passes: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_scope: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_review: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synergy_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch_with: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_note: Option<String>,
}

/// JSON structure for the exported PRD file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedPrd {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority_philosophy: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_acceptance_criteria: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_guidelines: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub user_stories: Vec<ExportedUserStory>,
}

/// Metadata loaded from prd_metadata table.
pub(crate) struct PrdMetadata {
    pub project: String,
    pub branch_name: Option<String>,
    pub description: Option<String>,
    pub priority_philosophy: Option<Value>,
    pub global_acceptance_criteria: Option<Value>,
    pub review_guidelines: Option<Value>,
    pub default_model: Option<String>,
}

/// Load PRD metadata from the database.
pub(crate) fn load_prd_metadata(conn: &Connection) -> TaskMgrResult<PrdMetadata> {
    // Check if metadata exists
    let count: i32 = conn.query_row("SELECT COUNT(*) FROM prd_metadata", [], |row| row.get(0))?;

    if count == 0 {
        // Return defaults if no metadata
        return Ok(PrdMetadata {
            project: "unknown".to_string(),
            branch_name: None,
            description: None,
            priority_philosophy: None,
            global_acceptance_criteria: None,
            review_guidelines: None,
            default_model: None,
        });
    }

    conn.query_row(
        r#"SELECT project, branch_name, description,
           priority_philosophy, global_acceptance_criteria, review_guidelines,
           default_model
           FROM prd_metadata WHERE id = 1"#,
        [],
        |row| {
            let project: String = row.get(0)?;
            let branch_name: Option<String> = row.get(1)?;
            let description: Option<String> = row.get(2)?;
            let priority_str: Option<String> = row.get(3)?;
            let global_str: Option<String> = row.get(4)?;
            let review_str: Option<String> = row.get(5)?;
            let default_model: Option<String> = row.get(6)?;

            // Parse JSON strings back to Values
            let priority_philosophy = priority_str.and_then(|s| serde_json::from_str(&s).ok());
            let global_acceptance_criteria = global_str.and_then(|s| serde_json::from_str(&s).ok());
            let review_guidelines = review_str.and_then(|s| serde_json::from_str(&s).ok());

            Ok(PrdMetadata {
                project,
                branch_name,
                description,
                priority_philosophy,
                global_acceptance_criteria,
                review_guidelines,
                default_model,
            })
        },
    )
    .map_err(Into::into)
}

/// Load all tasks with their files and relationships.
pub(crate) fn load_tasks(conn: &Connection) -> TaskMgrResult<Vec<ExportedUserStory>> {
    // Load all tasks ordered by ID
    let mut stmt = conn.prepare(
        r#"SELECT id, title, description, priority, status, notes,
           acceptance_criteria, review_scope, severity, source_review,
           model, difficulty, escalation_note
           FROM tasks ORDER BY id"#,
    )?;

    let task_rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let title: String = row.get(1)?;
        let description: Option<String> = row.get(2)?;
        let priority: i32 = row.get(3)?;
        let status_str: String = row.get(4)?;
        let notes: Option<String> = row.get(5)?;
        let acceptance_criteria_str: Option<String> = row.get(6)?;
        let review_scope_str: Option<String> = row.get(7)?;
        let severity: Option<String> = row.get(8)?;
        let source_review: Option<String> = row.get(9)?;
        let model: Option<String> = row.get(10)?;
        let difficulty: Option<String> = row.get(11)?;
        let escalation_note: Option<String> = row.get(12)?;

        Ok((
            id,
            title,
            description,
            priority,
            status_str,
            notes,
            acceptance_criteria_str,
            review_scope_str,
            severity,
            source_review,
            model,
            difficulty,
            escalation_note,
        ))
    })?;

    // Load all files into a map
    let files_map = load_all_task_files(conn)?;

    // Load all relationships into maps
    let (depends_on_map, synergy_with_map, batch_with_map, conflicts_with_map) =
        load_all_relationships(conn)?;

    let mut tasks = Vec::new();
    for row in task_rows {
        let (
            id,
            title,
            description,
            priority,
            status_str,
            notes,
            acceptance_criteria_str,
            review_scope_str,
            severity,
            source_review,
            model,
            difficulty,
            escalation_note,
        ) = row?;

        // Map status to passes boolean
        let status = TaskStatus::from_str(&status_str).unwrap_or(TaskStatus::Todo);
        let passes = status == TaskStatus::Done;

        // Parse acceptance_criteria from JSON
        let acceptance_criteria: Vec<String> = acceptance_criteria_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // Parse review_scope from JSON
        let review_scope: Option<Value> =
            review_scope_str.and_then(|s| serde_json::from_str(&s).ok());

        // Get files and relationships, sorted alphabetically
        let mut touches_files = files_map.get(&id).cloned().unwrap_or_default();
        touches_files.sort();

        let mut depends_on = depends_on_map.get(&id).cloned().unwrap_or_default();
        depends_on.sort();

        let mut synergy_with = synergy_with_map.get(&id).cloned().unwrap_or_default();
        synergy_with.sort();

        let mut batch_with = batch_with_map.get(&id).cloned().unwrap_or_default();
        batch_with.sort();

        let mut conflicts_with = conflicts_with_map.get(&id).cloned().unwrap_or_default();
        conflicts_with.sort();

        tasks.push(ExportedUserStory {
            id,
            title,
            description,
            priority,
            passes,
            notes,
            acceptance_criteria,
            review_scope,
            severity,
            source_review,
            touches_files,
            depends_on,
            synergy_with,
            batch_with,
            conflicts_with,
            model,
            difficulty,
            escalation_note,
        });
    }

    Ok(tasks)
}

/// Load all task files into a map.
pub(crate) fn load_all_task_files(
    conn: &Connection,
) -> TaskMgrResult<HashMap<String, Vec<String>>> {
    let mut stmt =
        conn.prepare("SELECT task_id, file_path FROM task_files ORDER BY task_id, file_path")?;
    let rows = stmt.query_map([], |row| {
        let task_id: String = row.get(0)?;
        let file_path: String = row.get(1)?;
        Ok((task_id, file_path))
    })?;

    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (task_id, file_path) = row?;
        map.entry(task_id).or_default().push(file_path);
    }

    Ok(map)
}

/// Load all task relationships into maps by type.
pub(crate) fn load_all_relationships(conn: &Connection) -> TaskMgrResult<RelationshipMaps> {
    let mut stmt = conn.prepare(
        "SELECT task_id, related_id, rel_type FROM task_relationships ORDER BY task_id, related_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let task_id: String = row.get(0)?;
        let related_id: String = row.get(1)?;
        let rel_type: String = row.get(2)?;
        Ok((task_id, related_id, rel_type))
    })?;

    let mut depends_on: HashMap<String, Vec<String>> = HashMap::new();
    let mut synergy_with: HashMap<String, Vec<String>> = HashMap::new();
    let mut batch_with: HashMap<String, Vec<String>> = HashMap::new();
    let mut conflicts_with: HashMap<String, Vec<String>> = HashMap::new();

    for row in rows {
        let (task_id, related_id, rel_type) = row?;
        match rel_type.as_str() {
            "dependsOn" => depends_on.entry(task_id).or_default().push(related_id),
            "synergyWith" => synergy_with.entry(task_id).or_default().push(related_id),
            "batchWith" => batch_with.entry(task_id).or_default().push(related_id),
            "conflictsWith" => conflicts_with.entry(task_id).or_default().push(related_id),
            _ => {}
        }
    }

    Ok((depends_on, synergy_with, batch_with, conflicts_with))
}

// Import FromStr implementations from models
use std::str::FromStr;
