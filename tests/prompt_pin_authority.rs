use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

use task_mgr::commands::{init, next};
use task_mgr::db::open_connection;
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::prompt::{BuildPromptParams, build_prompt};

const STALE_NEXT_CLAIM_SNIPPET: &str = "task-mgr next --prefix $PREFIX --claim";
const STALE_BASE_PROMPT: &str = r#"# Agent Instructions

## Your Task (every iteration)

1. Resolve the PRD prefix and claim work:

   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/example.json)
   task-mgr next --prefix $PREFIX --claim
   ```

   You don't pick — you claim what it returns.

2. This stale fixture also contains a tempting task-shaped value:

   ```json
   { "id": "STALE-BASE-999" }
   ```
"#;

#[test]
fn build_prompt_pins_single_task_and_orders_authority_guards_around_stale_base_prompt() {
    let (_tmp, conn, base_prompt_path) = setup_two_task_fixture();

    assert_eq!(
        in_progress_count(&conn),
        0,
        "fixture starts with two todo tasks"
    );

    let result = render_prompt(&conn, &base_prompt_path);
    let prompt = result.prompt;

    assert_eq!(
        in_progress_count(&conn),
        1,
        "a single build_prompt call must claim exactly one task"
    );

    let pinned_id = current_task_id_from_json_block(&prompt);
    assert_eq!(
        pinned_id, result.task_id,
        "pinned id must be parsed from the ## Current Task JSON block"
    );
    assert_ne!(
        pinned_id, "STALE-BASE-999",
        "the test must not accept ids from the stale base-prompt body"
    );

    let task_ops_pos = prompt
        .find("## Task lifecycle")
        .expect("task_ops is present");
    let task_ops_next_claim_pos = prompt[task_ops_pos..]
        .find("NEVER run `task-mgr next --claim`")
        .map(|offset| task_ops_pos + offset)
        .expect("early task_ops next --claim prohibition is present");
    let stale_next_claim_pos = prompt
        .find(STALE_NEXT_CLAIM_SNIPPET)
        .expect("stale base-prompt next --claim snippet is present");
    assert!(
        task_ops_next_claim_pos < stale_next_claim_pos,
        "task_ops prohibition (byte {task_ops_next_claim_pos}) must appear before stale next \
         --claim snippet (byte {stale_next_claim_pos})"
    );

    let base_prompt_pos = prompt
        .find("# Agent Instructions")
        .expect("base prompt body is present");
    let pin_authority_pos = prompt
        .find("## Task selection authority")
        .expect("late pin_authority section is present");
    assert!(
        base_prompt_pos < pin_authority_pos,
        "pin_authority (byte {pin_authority_pos}) must appear after base prompt body \
         (byte {base_prompt_pos})"
    );
}

#[test]
fn stale_agent_next_claim_would_claim_a_second_task_after_engine_pin() {
    let (tmp, conn, base_prompt_path) = setup_two_task_fixture();

    let result = render_prompt(&conn, &base_prompt_path);
    assert_eq!(in_progress_count(&conn), 1);

    let next_result =
        next::next(tmp.path(), &[], true, None, false, None).expect("stale next --claim succeeds");
    let claimed = next_result
        .task
        .expect("stale next --claim returns a second task");
    assert_ne!(
        claimed.id, result.task_id,
        "CLI next --claim must skip the engine-pinned in_progress task"
    );
    assert_eq!(
        in_progress_count(&conn),
        2,
        "document the bug class: stale next --claim creates a second in_progress task"
    );
}

fn setup_two_task_fixture() -> (TempDir, Connection, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let prd_path = tmp.path().join("pin-authority-prd.json");
    fs::write(&prd_path, two_task_prd_json()).expect("write PRD fixture");

    init::init(
        tmp.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .expect("init two-task fixture");

    let base_prompt_path = tmp.path().join("prompt.md");
    fs::write(&base_prompt_path, STALE_BASE_PROMPT).expect("write stale base prompt");

    let conn = open_connection(tmp.path()).expect("open db");
    (tmp, conn, base_prompt_path)
}

fn two_task_prd_json() -> String {
    serde_json::json!({
        "branchName": "feature/pin-authority-test",
        "description": "Two task PRD for pin-authority integration coverage.",
        "project": "pin-authority-fixture",
        "userStories": [
            {
                "acceptanceCriteria": ["First task can be pinned"],
                "batchWith": [],
                "conflictsWith": [],
                "dependsOn": [],
                "description": "Higher priority task.",
                "id": "PIN-AUTH-001",
                "notes": "Fixture task one.",
                "passes": false,
                "priority": 9,
                "synergyWith": [],
                "title": "First pin authority task",
                "touchesFiles": []
            },
            {
                "acceptanceCriteria": ["Second task remains todo until stale next claim"],
                "batchWith": [],
                "conflictsWith": [],
                "dependsOn": [],
                "description": "Lower priority task.",
                "id": "PIN-AUTH-002",
                "notes": "Fixture task two.",
                "passes": false,
                "priority": 1,
                "synergyWith": [],
                "title": "Second pin authority task",
                "touchesFiles": []
            }
        ]
    })
    .to_string()
}

fn render_prompt(
    conn: &Connection,
    base_prompt_path: &Path,
) -> task_mgr::loop_engine::prompt::PromptResult {
    let permission_mode = PermissionMode::Dangerous;
    let params = BuildPromptParams {
        dir: base_prompt_path.parent().expect("base prompt parent"),
        project_root: base_prompt_path.parent().expect("base prompt parent"),
        conn,
        after_files: &[],
        run_id: None,
        iteration: 1,
        reorder_hint: None,
        session_guidance: "",
        base_prompt_path,
        steering_path: None,
        verbose: false,
        default_model: None,
        task_prefix: None,
        batch_sibling_prds: &[],
        permission_mode: &permission_mode,
        models_config: task_mgr::loop_engine::project_config::default_models_config(),
        routing_config: task_mgr::loop_engine::project_config::default_routing_config(),
        provider_blackouts: Default::default(),
        excluded_ids: Default::default(),
    };

    build_prompt(&params)
        .expect("build_prompt returned Err")
        .expect("build_prompt returned None")
}

fn in_progress_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE status = 'in_progress'",
        [],
        |row| row.get(0),
    )
    .expect("count in_progress tasks")
}

fn current_task_id_from_json_block(prompt: &str) -> String {
    let current_task_pos = prompt
        .find("## Current Task")
        .expect("prompt has ## Current Task marker");
    let after_marker = &prompt[current_task_pos..];
    let first_fence = after_marker
        .find("```json")
        .expect("current task section has json fence");
    let json_start = current_task_pos + first_fence + "```json".len();
    let after_json_start = &prompt[json_start..];
    let fence_end = after_json_start
        .find("```")
        .expect("current task json fence is closed");
    let json_block = &after_json_start[..fence_end];

    let value: Value = serde_json::from_str(json_block).expect("current task block is valid JSON");
    value
        .get("id")
        .and_then(Value::as_str)
        .expect("current task JSON has string id")
        .to_string()
}
