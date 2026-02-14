//! Output types and formatting for the fail command.

use serde::Serialize;

use crate::models::TaskStatus;

/// Result of failing a single task.
#[derive(Debug, Clone, Serialize)]
pub struct TaskFailResult {
    /// The task that was marked as failed
    pub task_id: String,
    /// Previous status before failure
    pub previous_status: TaskStatus,
    /// The new status set
    pub new_status: TaskStatus,
    /// Error message if provided
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Updated error count
    pub error_count: i32,
    /// Next steps hint
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_steps: Option<String>,
}

/// Result of failing multiple tasks.
#[derive(Debug, Clone, Serialize)]
pub struct FailResult {
    /// Results for each task
    pub tasks: Vec<TaskFailResult>,
    /// Number of tasks successfully marked as failed
    pub failed_count: usize,
    /// Run ID if tracking was enabled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// Format fail result as human-readable text.
#[must_use]
pub fn format_text(result: &FailResult) -> String {
    let mut output = String::new();

    if result.tasks.len() == 1 {
        // Single task output
        let task = &result.tasks[0];
        output.push_str(&format!(
            "Marked task {} as {} (was {}).\n",
            task.task_id, task.new_status, task.previous_status
        ));

        if let Some(ref err) = task.error {
            output.push_str(&format!("Error: {}\n", err));
        }

        output.push_str(&format!("Error count: {}\n", task.error_count));

        if let Some(ref hint) = task.next_steps {
            output.push_str(&format!("Next steps: {}\n", hint));
        }
    } else {
        // Multiple tasks output
        output.push_str(&format!(
            "Marked {} task(s) as failed.\n",
            result.failed_count
        ));
        for task in &result.tasks {
            output.push_str(&format!(
                "  - {} ({} → {})",
                task.task_id, task.previous_status, task.new_status
            ));
            if let Some(ref err) = task.error {
                output.push_str(&format!(", error: {}", err));
            }
            output.push('\n');
        }
    }

    if let Some(ref rid) = result.run_id {
        output.push_str(&format!("Run: {}\n", rid));
    }

    output
}
