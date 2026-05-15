use serde_json::Value;
use std::fs;
use task_mgr::commands::add::add;
use task_mgr::commands::init::{PrefixMode, init};
use tempfile::TempDir;

const ACTIVE_PREFIX_ENV: &str = "TASK_MGR_ACTIVE_PREFIX";

static ENV_PREFIX_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: clears `TASK_MGR_ACTIVE_PREFIX` for the duration of the test,
/// then restores it on drop.  Held alongside `ENV_PREFIX_MUTEX` so concurrent
/// tests don't race on env-var state.
///
/// The loop engine exports this var when running task-mgr inside a loop; it
/// leaks into the cargo-test child env and causes `add()` (via
/// `resolve_active_prefix`) to fail with "stale pin" because the leaked prefix
/// isn't registered in the test fixtures' fresh DBs.
struct EnvIsolation {
    _lock: std::sync::MutexGuard<'static, ()>,
    prior: Option<String>,
}

impl EnvIsolation {
    fn new() -> Self {
        let lock = ENV_PREFIX_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(ACTIVE_PREFIX_ENV).ok();
        unsafe { std::env::remove_var(ACTIVE_PREFIX_ENV) };
        Self { _lock: lock, prior }
    }
}

impl Drop for EnvIsolation {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var(ACTIVE_PREFIX_ENV, v) },
            None => unsafe { std::env::remove_var(ACTIVE_PREFIX_ENV) },
        }
    }
}

fn minimal_prd_json(p1: i32, p2: i32) -> String {
    serde_json::json!({
        "project": "test-proj",
        "userStories": [
            {"id": "SEED-001", "title": "first seed", "priority": p1, "passes": false},
            {"id": "SEED-002", "title": "second seed", "priority": p2, "passes": false}
        ]
    })
    .to_string()
}

/// Write PRD JSON to temp dir and run `init`, returning (TempDir, prd_path).
/// The PRD file is placed at the TempDir root (outside the `tasks/` subdir)
/// so init stores its absolute path in prd_files.
fn setup_with_prd(prd_json: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let prd_path = dir.path().join("test_prd.json");
    fs::write(&prd_path, prd_json).unwrap();
    init(
        dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();
    (dir, prd_path)
}

// ---------------------------------------------------------------------------
// test_add_roundtrips_task_into_prd_json
// ---------------------------------------------------------------------------

#[test]
fn test_add_roundtrips_task_into_prd_json() {
    let _env = EnvIsolation::new();
    let (dir, prd_path) = setup_with_prd(&minimal_prd_json(50, 100));

    let new_task = serde_json::json!({
        "id": "NEW-001",
        "title": "new task",
        "touchesFiles": ["src/main.rs"],
        "dependsOn": ["SEED-001"]
    })
    .to_string();

    let result = add(dir.path(), &new_task, None, &[]).unwrap();
    assert_eq!(result.task_id, "NEW-001");
    assert_eq!(result.priority, 49, "priority must be top (50) - 1");
    assert!(
        result.prd_path.is_some(),
        "prd_path must be set after successful sync"
    );

    // --- DB assertions ---
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();

    let (title, priority, status): (String, i32, String) = conn
        .query_row(
            "SELECT title, priority, status FROM tasks WHERE id = 'NEW-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "new task");
    assert_eq!(priority, 49);
    assert_eq!(status, "todo");

    let rel_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships \
             WHERE task_id = 'NEW-001' AND rel_type = 'dependsOn'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rel_count, 1, "dependsOn relationship must be recorded");

    let file_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_files WHERE task_id = 'NEW-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(file_count, 1, "touches_files row must be recorded");

    // --- PRD JSON assertions ---
    let updated_json = fs::read_to_string(&prd_path).unwrap();
    let updated: Value = serde_json::from_str(&updated_json).unwrap();
    let stories = updated["userStories"].as_array().unwrap();
    assert_eq!(stories.len(), 3, "must have 3 tasks after add");

    // New task is appended last.
    assert_eq!(stories[2]["id"].as_str().unwrap(), "NEW-001");
    assert_eq!(stories[2]["title"].as_str().unwrap(), "new task");
    assert_eq!(stories[2]["priority"].as_i64().unwrap(), 49);

    // Original task JSON fields must be byte-for-byte equal to the seed values.
    let original: Value = serde_json::from_str(&minimal_prd_json(50, 100)).unwrap();
    let orig_stories = original["userStories"].as_array().unwrap();
    assert_eq!(
        &stories[0], &orig_stories[0],
        "first original task must be unchanged"
    );
    assert_eq!(
        &stories[1], &orig_stories[1],
        "second original task must be unchanged"
    );
}

// ---------------------------------------------------------------------------
// test_auto_priority_derives_from_top
// ---------------------------------------------------------------------------

#[test]
fn test_auto_priority_derives_from_top() {
    let _env = EnvIsolation::new();
    // Seeds: priorities [50, 100].
    // Top task (selected first by select_next_task) is priority 50.
    // New task must get 49, NOT 99 (one less than the other task).
    let (dir, _) = setup_with_prd(&minimal_prd_json(50, 100));

    // Use a 2-segment ID (no prefix) so select_next_task sees all SEED tasks.
    let new_task = serde_json::json!({"id": "PRIO-001", "title": "priority check"}).to_string();

    let result = add(dir.path(), &new_task, None, &[]).unwrap();
    assert_eq!(
        result.priority, 49,
        "priority must be 50 (top) - 1 = 49, not 99 (second task - 1)"
    );
}

// ---------------------------------------------------------------------------
// test_no_tmp_file_left_after_successful_add
// ---------------------------------------------------------------------------

#[test]
fn test_no_tmp_file_left_after_successful_add() {
    let _env = EnvIsolation::new();
    let (dir, prd_path) = setup_with_prd(&minimal_prd_json(50, 100));

    let new_task = serde_json::json!({"id": "NEW-TMP-001", "title": "tmp file check"}).to_string();
    add(dir.path(), &new_task, None, &[]).unwrap();

    // atomic_write uses format!(".{filename}.task-mgr-add.tmp")
    let prd_filename = prd_path.file_name().unwrap().to_str().unwrap();
    let tmp_name = format!(".{}.task-mgr-add.tmp", prd_filename);
    let tmp_path = prd_path.parent().unwrap().join(&tmp_name);

    assert!(
        !tmp_path.exists(),
        "tmp file {} must not exist after successful add",
        tmp_path.display()
    );
}

// ---------------------------------------------------------------------------
// Edge case: userStories: [] (empty array)
// ---------------------------------------------------------------------------

#[test]
fn test_add_with_empty_user_stories() {
    let _env = EnvIsolation::new();
    let empty_prd = serde_json::json!({
        "project": "test-proj",
        "userStories": []
    })
    .to_string();
    let (dir, prd_path) = setup_with_prd(&empty_prd);

    let new_task = serde_json::json!({
        "id": "EMPTY-001",
        "title": "first task in empty prd",
        "priority": 100
    })
    .to_string();

    let result = add(dir.path(), &new_task, None, &[]).unwrap();
    assert_eq!(result.task_id, "EMPTY-001");

    let updated: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    let stories = updated["userStories"].as_array().unwrap();
    assert_eq!(
        stories.len(),
        1,
        "must have exactly 1 task after add to empty prd"
    );
    assert_eq!(stories[0]["id"].as_str().unwrap(), "EMPTY-001");
}

// ---------------------------------------------------------------------------
// Failure mode: PRD JSON deleted after init but before add
// ---------------------------------------------------------------------------

#[test]
fn test_add_succeeds_when_prd_json_deleted() {
    let _env = EnvIsolation::new();
    let (dir, prd_path) = setup_with_prd(&minimal_prd_json(50, 100));

    // Remove the PRD JSON to simulate deletion between init and add.
    fs::remove_file(&prd_path).unwrap();

    let new_task = serde_json::json!({"id": "ORPHAN-001", "title": "orphaned task"}).to_string();

    // add() must succeed: DB gets the row even though PRD JSON is gone.
    // The implementation logs a warning and continues — it does NOT error.
    let result = add(dir.path(), &new_task, None, &[]).unwrap();
    assert_eq!(result.task_id, "ORPHAN-001");

    // DB must have the row regardless of the failed file sync.
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = 'ORPHAN-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "task must be in DB even though PRD JSON was missing"
    );
}

// ---------------------------------------------------------------------------
// --depended-on-by integration tests (FEAT-006)
// ---------------------------------------------------------------------------

#[test]
fn test_depended_on_by_updates_prd_json_dependson() {
    let _env = EnvIsolation::new();
    // Seed a PRD where SEED-001 has no dependsOn. After add with
    // --depended-on-by SEED-001, SEED-001's userStories entry must gain
    // the new task id in its dependsOn array.
    let (dir, prd_path) = setup_with_prd(&minimal_prd_json(50, 100));

    let new_task = serde_json::json!({"id": "CHILD-001", "title": "child"}).to_string();

    add(dir.path(), &new_task, None, &["SEED-001".to_string()]).unwrap();

    // --- PRD JSON assertions: existing entry's dependsOn now contains the new id ---
    let updated: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    let stories = updated["userStories"].as_array().unwrap();

    let seed = stories
        .iter()
        .find(|s| s["id"].as_str() == Some("SEED-001"))
        .expect("SEED-001 entry preserved");
    let deps = seed["dependsOn"]
        .as_array()
        .expect("dependsOn array created on SEED-001");
    assert!(
        deps.iter().any(|v| v.as_str() == Some("CHILD-001")),
        "SEED-001.dependsOn must contain CHILD-001, got: {:?}",
        deps
    );

    // --- New task's OWN dependsOn must NOT contain SEED-001 (direction reversed) ---
    let child = stories
        .iter()
        .find(|s| s["id"].as_str() == Some("CHILD-001"))
        .expect("CHILD-001 appended");
    let child_deps = child
        .get("dependsOn")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !child_deps.iter().any(|v| v.as_str() == Some("SEED-001")),
        "new task's dependsOn must NOT contain the --depended-on-by target (direction reversed)"
    );

    // --- DB assertion: reverse row exists ---
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let rel: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships \
             WHERE task_id = 'SEED-001' AND related_id = 'CHILD-001' AND rel_type = 'dependsOn'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rel, 1, "DB reverse dependsOn row must exist");
}

#[test]
fn test_depended_on_by_missing_target_in_prd_json_logs_but_dbstill_updated() {
    let _env = EnvIsolation::new();
    // Scenario: target exists in the DB (so pre-flight validation passes)
    // but is MISSING from the on-disk PRD JSON (out-of-band edit). The DB
    // write must commit, a warning logs for the missing target, and the
    // command returns OK.
    let (dir, prd_path) = setup_with_prd(&minimal_prd_json(50, 100));

    // Simulate out-of-band JSON edit: remove SEED-002 from userStories but
    // leave it in the DB.
    let mut json: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    let arr = json["userStories"].as_array_mut().unwrap();
    arr.retain(|s| s["id"].as_str() != Some("SEED-002"));
    fs::write(&prd_path, serde_json::to_string_pretty(&json).unwrap()).unwrap();

    let new_task = serde_json::json!({"id": "LINKED-001", "title": "linked"}).to_string();

    // SEED-002 exists in DB, so pre-flight passes. Command should return OK.
    let result = add(dir.path(), &new_task, None, &["SEED-002".to_string()]).unwrap();
    assert_eq!(result.task_id, "LINKED-001");

    // --- DB assertions: new task inserted, reverse link recorded ---
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let new_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = 'LINKED-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(new_count, 1, "new task must be in DB");

    let rel: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships \
             WHERE task_id = 'SEED-002' AND related_id = 'LINKED-001' AND rel_type = 'dependsOn'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        rel, 1,
        "DB reverse row must commit even if PRD JSON lacks the target"
    );

    // --- PRD JSON: new task is appended; SEED-002 still absent ---
    let updated: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    let stories = updated["userStories"].as_array().unwrap();
    assert!(
        stories
            .iter()
            .any(|s| s["id"].as_str() == Some("LINKED-001")),
        "new task must be appended to PRD JSON"
    );
    assert!(
        !stories.iter().any(|s| s["id"].as_str() == Some("SEED-002")),
        "SEED-002 entry should still be absent from the JSON we edited"
    );
}
