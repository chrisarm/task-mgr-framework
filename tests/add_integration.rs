use serde_json::Value;
use std::fs;
use task_mgr::commands::add::add;
use task_mgr::commands::init::{PrefixMode, init};
use task_mgr::commands::update::update;
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

    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
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

    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
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
    add(dir.path(), &new_task, None, &[], None).unwrap();

    // unique_tmp_path uses `.{filename}.{pid}-{n}-{nanos}.tmp`; none may remain.
    let leftovers: Vec<_> = fs::read_dir(prd_path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no .tmp files may remain after successful add, found: {leftovers:?}"
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

    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
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
    // Remap-then-is_file() skips when neither path is a regular file
    // (FEAT-004b) — it does NOT error or invent a path.
    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
    assert_eq!(result.task_id, "ORPHAN-001");
    assert!(
        result.prd_path.is_none(),
        "neither remapped nor registered is a file → skip JSON sync"
    );

    // DB must have the row regardless of the skipped file sync.
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

    add(dir.path(), &new_task, None, &["SEED-001".to_string()], None).unwrap();

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
    let result = add(dir.path(), &new_task, None, &["SEED-002".to_string()], None).unwrap();
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

/// Run `init` with an explicit prefix so the DB stores prefixed ids while the
/// on-disk JSON keeps its unprefixed convention (Explicit mode does not rewrite
/// userStory ids). Mirrors `setup_with_prd` otherwise.
fn setup_with_prefixed_prd(prd_json: &str, prefix: &str) -> (TempDir, std::path::PathBuf) {
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
        PrefixMode::Explicit(prefix.to_string()),
    )
    .unwrap();
    (dir, prd_path)
}

#[test]
fn test_depended_on_by_prefixed_db_unprefixed_json_sync() {
    // Regression for the eks-inventory bug (learning #1087): DB stores prefixed
    // ids, the PRD JSON stores them unprefixed. `add --depended-on-by
    // e474b6f2-MILESTONE-1` must (a) write the new entry with an UNPREFIXED id
    // and (b) actually sync MILESTONE-1's dependsOn — previously the prefixed
    // target failed to match the unprefixed JSON entry and was silently skipped.
    let _env = EnvIsolation::new();
    let prd = serde_json::json!({
        "project": "eks-test",
        "userStories": [
            {"id": "MILESTONE-1", "title": "milestone", "priority": 10, "passes": false},
            {"id": "FEAT-1", "title": "feature one", "priority": 20, "passes": false}
        ]
    })
    .to_string();
    let (dir, prd_path) = setup_with_prefixed_prd(&prd, "e474b6f2");

    // New task carries its own (bare) dependsOn to verify relationship-array
    // stripping on the appended entry too.
    let new_task = serde_json::json!({
        "id": "CODE-REVIEW-2",
        "title": "review",
        "dependsOn": ["FEAT-1"]
    })
    .to_string();

    let result = add(
        dir.path(),
        &new_task,
        None,
        &["e474b6f2-MILESTONE-1".to_string()],
        None,
    )
    .unwrap();
    assert_eq!(
        result.task_id, "e474b6f2-CODE-REVIEW-2",
        "DB task id must carry the active prefix"
    );

    // --- DB: prefixed task + reverse edge + own dependsOn edge ---
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let task_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = 'e474b6f2-CODE-REVIEW-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(task_count, 1, "new task stored with prefixed id");

    let reverse: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships \
             WHERE task_id = 'e474b6f2-MILESTONE-1' \
               AND related_id = 'e474b6f2-CODE-REVIEW-2' AND rel_type = 'dependsOn'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        reverse, 1,
        "reverse dependsOn edge recorded with prefixed ids"
    );

    let own_dep: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships \
             WHERE task_id = 'e474b6f2-CODE-REVIEW-2' \
               AND related_id = 'e474b6f2-FEAT-1' AND rel_type = 'dependsOn'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        own_dep, 1,
        "new task's own dependsOn stored with prefixed ids"
    );

    // --- PRD JSON: ids stay in the unprefixed convention ---
    let updated: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    let stories = updated["userStories"].as_array().unwrap();

    let child = stories
        .iter()
        .find(|s| s["id"].as_str() == Some("CODE-REVIEW-2"))
        .expect("new entry must be written with the UNPREFIXED id");
    let child_deps = child["dependsOn"]
        .as_array()
        .expect("child dependsOn array");
    assert!(
        child_deps.iter().any(|v| v.as_str() == Some("FEAT-1")),
        "new entry's own dependsOn must be unprefixed (FEAT-1), got: {child_deps:?}"
    );
    assert!(
        !child_deps
            .iter()
            .any(|v| v.as_str() == Some("e474b6f2-FEAT-1")),
        "new entry's dependsOn must NOT carry the prefix"
    );

    let milestone = stories
        .iter()
        .find(|s| s["id"].as_str() == Some("MILESTONE-1"))
        .expect("MILESTONE-1 entry preserved");
    let deps = milestone["dependsOn"]
        .as_array()
        .expect("MILESTONE-1 dependsOn synced");
    assert!(
        deps.iter().any(|v| v.as_str() == Some("CODE-REVIEW-2")),
        "MILESTONE-1.dependsOn must gain the unprefixed new id, got: {deps:?}"
    );
}

#[test]
fn test_depended_on_by_mixed_form_json_still_matches() {
    // Defence for files already polluted by the old bug: an entry stored with a
    // PREFIXED id must still be matched (prefix_id is idempotent on import, so
    // the DB id is identical), and the new entry/sync still use the base id.
    let _env = EnvIsolation::new();
    let prd = serde_json::json!({
        "project": "eks-test",
        "userStories": [
            {"id": "e474b6f2-MILESTONE-1", "title": "milestone", "priority": 10, "passes": false}
        ]
    })
    .to_string();
    let (dir, prd_path) = setup_with_prefixed_prd(&prd, "e474b6f2");

    let new_task = serde_json::json!({"id": "CODE-REVIEW-3", "title": "review"}).to_string();
    add(
        dir.path(),
        &new_task,
        None,
        &["e474b6f2-MILESTONE-1".to_string()],
        None,
    )
    .unwrap();

    let updated: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    let stories = updated["userStories"].as_array().unwrap();

    let milestone = stories
        .iter()
        .find(|s| s["id"].as_str() == Some("e474b6f2-MILESTONE-1"))
        .expect("prefixed milestone entry preserved");
    let deps = milestone["dependsOn"]
        .as_array()
        .expect("dependsOn synced on the prefixed entry");
    assert!(
        deps.iter().any(|v| v.as_str() == Some("CODE-REVIEW-3")),
        "prefixed entry's dependsOn must gain the unprefixed new id, got: {deps:?}"
    );
    assert!(
        stories
            .iter()
            .any(|s| s["id"].as_str() == Some("CODE-REVIEW-3")),
        "new entry appended with the unprefixed id"
    );
}

// ---------------------------------------------------------------------------
// FEAT-004b: single write path / remap-then-is_file / drop LIMIT 1
// ---------------------------------------------------------------------------

#[test]
fn test_default_add_writes_sole_registered_file_when_present() {
    // ctx is None (--no-prefix / zero non-NULL prefixes) → sole task_list
    // row uses remap-then-is_file; absolute registered file is written.
    let _env = EnvIsolation::new();
    let (dir, prd_path) = setup_with_prd(&minimal_prd_json(50, 100));
    let before = fs::read_to_string(&prd_path).unwrap();

    let new_task = serde_json::json!({"id": "SYNC-001", "title": "sync me"}).to_string();
    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
    assert_eq!(result.task_id, "SYNC-001");
    assert_eq!(
        result.prd_path.as_ref().and_then(|p| p.canonicalize().ok()),
        prd_path.canonicalize().ok(),
        "display/write path must be the registered file"
    );

    let after = fs::read_to_string(&prd_path).unwrap();
    assert_ne!(before, after, "registered file must be mutated");
    let parsed: Value = serde_json::from_str(&after).unwrap();
    let stories = parsed["userStories"].as_array().unwrap();
    assert!(
        stories.iter().any(|s| s["id"].as_str() == Some("SYNC-001")),
        "new story must land in the registered JSON"
    );
}

#[test]
fn test_default_add_single_prefix_display_equals_write_path() {
    let _env = EnvIsolation::new();
    let (dir, prd_path) = setup_with_prefixed_prd(&minimal_prd_json(50, 100), "SP");

    let new_task = serde_json::json!({"id": "FIX-001", "title": "prefixed"}).to_string();
    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
    assert_eq!(result.task_id, "SP-FIX-001");
    assert_eq!(
        result.prd_path.as_ref().and_then(|p| p.canonicalize().ok()),
        prd_path.canonicalize().ok(),
        "ctx.prd_json_path (display) must equal the append write path"
    );
    let parsed: Value = serde_json::from_str(&fs::read_to_string(&prd_path).unwrap()).unwrap();
    // Explicit-prefix init keeps JSON unprefixed; append strips DB prefix.
    assert!(
        parsed["userStories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_str() == Some("FIX-001")),
        "JSON must gain unprefixed FIX-001, got: {parsed}"
    );
}

#[test]
fn test_ctx_none_two_task_lists_skips_json_sync() {
    // Architect tightening: count==1 only. Two registered task_lists with
    // NULL prefixes → ctx None and sole_task_list_path returns None → skip.
    let _env = EnvIsolation::new();
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    let prd_a = serde_json::json!({
        "project": "a",
        "userStories": [
            {"id": "A-SEED-001", "title": "a seed", "priority": 50, "passes": false}
        ]
    })
    .to_string();
    let prd_b = serde_json::json!({
        "project": "b",
        "userStories": [
            {"id": "B-SEED-001", "title": "b seed", "priority": 40, "passes": false}
        ]
    })
    .to_string();
    fs::write(&a, &prd_a).unwrap();
    fs::write(&b, &prd_b).unwrap();
    init(
        dir.path(),
        &[&a, &b],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let before_a = fs::read_to_string(&a).unwrap();
    let before_b = fs::read_to_string(&b).unwrap();
    let new_task = serde_json::json!({"id": "MULTI-001", "title": "ambiguous"}).to_string();
    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
    assert_eq!(result.task_id, "MULTI-001");
    assert!(
        result.prd_path.is_none(),
        "must not LIMIT-1 into PRD #1 when count!=1"
    );
    assert_eq!(fs::read_to_string(&a).unwrap(), before_a);
    assert_eq!(fs::read_to_string(&b).unwrap(), before_b);

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = 'MULTI-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "DB insert still succeeds when JSON sync skips");
}

// ---------------------------------------------------------------------------
// FEAT-004: --from-json pin protocol
// ---------------------------------------------------------------------------

#[test]
fn test_from_json_pins_registered_file_and_appends() {
    let _env = EnvIsolation::new();
    let prd = serde_json::json!({
        "project": "pin-test",
        "taskPrefix": "PIN",
        "userStories": [
            {"id": "SEED-001", "title": "seed", "priority": 50, "passes": false}
        ]
    })
    .to_string();
    let dir = TempDir::new().unwrap();
    let prd_path = dir.path().join("tasks");
    fs::create_dir_all(&prd_path).unwrap();
    let prd_file = prd_path.join("pin.json");
    fs::write(&prd_file, &prd).unwrap();
    init(
        dir.path(),
        &[&prd_file],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("PIN".to_string()),
    )
    .unwrap();

    let new_task = serde_json::json!({"id": "FIX-001", "title": "fixup"}).to_string();
    let result = add(
        dir.path(),
        &new_task,
        None,
        &["PIN-SEED-001".to_string()],
        Some(&prd_file),
    )
    .unwrap();
    assert_eq!(
        result.task_id, "PIN-FIX-001",
        "non-empty prefix auto-prefixes"
    );
    assert_eq!(
        result.prd_path.as_ref().map(|p| p.canonicalize().unwrap()),
        Some(prd_file.canonicalize().unwrap()),
        "write target must be the canonical --from-json path"
    );

    let updated: Value = serde_json::from_str(&fs::read_to_string(&prd_file).unwrap()).unwrap();
    let stories = updated["userStories"].as_array().unwrap();
    assert!(
        stories
            .iter()
            .any(|s| s["id"].as_str() == Some("FIX-001") || s["id"].as_str() == Some("PIN-FIX-001")),
        "userStories must append the new task (JSON may store bare or prefixed id)"
    );

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let rel: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships \
             WHERE task_id = 'PIN-SEED-001' AND related_id = 'PIN-FIX-001' \
               AND rel_type = 'dependsOn'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rel, 1, "reverse --depended-on-by must be recorded");
}

#[test]
fn test_from_json_unregistered_no_db_row() {
    let _env = EnvIsolation::new();
    let (dir, _) = setup_with_prd(&minimal_prd_json(50, 100));
    let orphan = dir.path().join("never-inited.json");
    fs::write(
        &orphan,
        r#"{"project":"x","taskPrefix":"ORPHAN","userStories":[]}"#,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();

    let new_task = serde_json::json!({"id": "LEAK-001", "title": "must not insert"}).to_string();
    let err = add(dir.path(), &new_task, None, &[], Some(&orphan)).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("loop init"), "must name loop init: {msg}");
    assert!(
        msg.contains("not a registered task_list"),
        "unregistered copy: {msg}"
    );

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "unregistered pin must not insert a DB row");
}

#[test]
fn test_from_json_missing_path_no_db_row() {
    let _env = EnvIsolation::new();
    let (dir, _) = setup_with_prd(&minimal_prd_json(50, 100));
    let missing = dir.path().join("does-not-exist.json");

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();

    let new_task = serde_json::json!({"id": "LEAK-002", "title": "must not insert"}).to_string();
    let err = add(dir.path(), &new_task, None, &[], Some(&missing)).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "missing copy must be distinct: {msg}"
    );
    assert!(
        !msg.contains("not a registered task_list"),
        "missing must not look like unregistered: {msg}"
    );

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn test_from_json_directory_no_db_row() {
    let _env = EnvIsolation::new();
    let (dir, _) = setup_with_prd(&minimal_prd_json(50, 100));
    let as_dir = dir.path().join("a-directory");
    fs::create_dir_all(&as_dir).unwrap();

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();

    let new_task = serde_json::json!({"id": "LEAK-003", "title": "must not insert"}).to_string();
    let err = add(dir.path(), &new_task, None, &[], Some(&as_dir)).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not a regular file") || msg.contains("directory"),
        "directory copy must be distinct: {msg}"
    );
    assert!(!msg.contains("not a registered task_list"), "{msg}");
    assert!(!msg.contains("does not exist"), "{msg}");

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn test_from_json_cross_prd_depended_on_by_refuses_no_row() {
    let _env = EnvIsolation::new();
    // Two registered PRDs with distinct prefixes.
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"a","taskPrefix":"alpha","userStories":[{"id":"SEED-001","title":"a","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"b","taskPrefix":"beta","userStories":[{"id":"MILESTONE-1","title":"b","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    init(
        dir.path(),
        &[&a],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("alpha".to_string()),
    )
    .unwrap();
    init(
        dir.path(),
        &[&b],
        false,
        true, // append
        false,
        false,
        PrefixMode::Explicit("beta".to_string()),
    )
    .unwrap();

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    let meta_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
        .unwrap();
    let files_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
        .unwrap();

    let new_task = serde_json::json!({"id": "FIX-001", "title": "cross"}).to_string();
    let err = add(
        dir.path(),
        &new_task,
        None,
        &["beta-MILESTONE-1".to_string()],
        Some(&a),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("Refusing:"), "cross-PRD must refuse: {msg}");

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "refusal must not insert a task row");
    let meta_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
        .unwrap();
    let files_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        meta_before, meta_after,
        "--from-json must not register metadata"
    );
    assert_eq!(
        files_before, files_after,
        "--from-json must not register files"
    );
}

#[test]
fn test_from_json_null_prefix_inserted_id_is_feat_001_not_dash() {
    let _env = EnvIsolation::new();
    let prd = serde_json::json!({
        "project": "noprefix",
        "userStories": [
            {"id": "SEED-001", "title": "seed", "priority": 10, "passes": false}
        ]
    })
    .to_string();
    let (dir, prd_path) = setup_with_prd(&prd);

    let new_task = serde_json::json!({"id": "FEAT-001", "title": "null-prefix pin"}).to_string();
    let result = add(
        dir.path(),
        &new_task,
        None,
        &["SEED-001".to_string()],
        Some(&prd_path),
    )
    .unwrap();
    assert_eq!(
        result.task_id, "FEAT-001",
        "NULL-prefix --from-json must insert FEAT-001, not -FEAT-001"
    );
    assert!(!result.task_id.starts_with('-'));
}

#[test]
fn test_from_json_relative_prd_files_worktree_registered() {
    // Seed prd_files as init would with relative `tasks/foo.json` (never a bare
    // basename). Pin the worktree copy via absolute path — registration via
    // match (a) taskPrefix and/or pin-19 (b)/(c). Write target stays the flag
    // PATH (never remapped away). Path-identity (b)/(c) alone is covered by
    // `commands::context::tests::test_paths_identify_c_remapped_worktree_path`.
    let _env = EnvIsolation::new();

    let main = TempDir::new().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(main.path())
            .status()
            .unwrap()
            .success()
    );
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(main.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(main.path())
        .status();
    fs::create_dir_all(main.path().join("tasks")).unwrap();
    let main_prd = main.path().join("tasks/foo.json");
    fs::write(
        &main_prd,
        r#"{"project":"wt","taskPrefix":"WT","userStories":[{"id":"SEED-001","title":"s","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(main.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(main.path())
        .status();

    init(
        main.path(),
        &[&main_prd],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("WT".to_string()),
    )
    .unwrap();

    // Force the registered row to the init-shaped relative form the AC names.
    let conn = rusqlite::Connection::open(main.path().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE prd_files SET file_path = 'tasks/foo.json' WHERE file_type = 'task_list'",
        [],
    )
    .unwrap();
    let stored: String = conn
        .query_row(
            "SELECT file_path FROM prd_files WHERE file_type = 'task_list'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, "tasks/foo.json",
        "identity tests must seed relative tasks/foo.json, not a bare basename"
    );

    let wt_parent = TempDir::new().unwrap();
    let wt_path = wt_parent.path().join("wt");
    assert!(
        std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feat/from-json-pin",
                wt_path.to_str().unwrap(),
            ])
            .current_dir(main.path())
            .status()
            .unwrap()
            .success(),
        "worktree add must succeed"
    );

    let wt_prd = wt_path.join("tasks/foo.json");
    assert!(wt_prd.is_file(), "worktree must have tasks/foo.json");

    let new_task = serde_json::json!({"id": "FIX-001", "title": "wt pin"}).to_string();
    let result = add(main.path(), &new_task, None, &[], Some(wt_prd.as_path()))
        .expect("relative prd_files + worktree --from-json must be registered");
    assert_eq!(result.task_id, "WT-FIX-001");
    assert_eq!(
        result.prd_path.as_ref().map(|p| p.canonicalize().unwrap()),
        Some(wt_prd.canonicalize().unwrap()),
        "write target must be the worktree flag path (never remapped away)"
    );
}

#[test]
fn test_update_from_json_relative_prd_files_worktree_registered() {
    // FEAT-007: same relative `prd_files` + worktree `--from-json` pin as
    // `test_from_json_relative_prd_files_worktree_registered`, for update.
    // Write target stays the flag PATH (never remapped away). DB stays on the
    // main checkout path passed as db_dir (library init style).
    let _env = EnvIsolation::new();

    let main = TempDir::new().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(main.path())
            .status()
            .unwrap()
            .success()
    );
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(main.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(main.path())
        .status();
    fs::create_dir_all(main.path().join("tasks")).unwrap();
    let main_prd = main.path().join("tasks/foo.json");
    fs::write(
        &main_prd,
        r#"{"project":"wt","taskPrefix":"WT","userStories":[{"id":"SEED-001","title":"s","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(main.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(main.path())
        .status();

    init(
        main.path(),
        &[&main_prd],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("WT".to_string()),
    )
    .unwrap();

    // Force the registered row to the init-shaped relative form the AC names.
    let conn = rusqlite::Connection::open(main.path().join("tasks.db")).unwrap();
    conn.execute(
        "UPDATE prd_files SET file_path = 'tasks/foo.json' WHERE file_type = 'task_list'",
        [],
    )
    .unwrap();
    let stored: String = conn
        .query_row(
            "SELECT file_path FROM prd_files WHERE file_type = 'task_list'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        stored, "tasks/foo.json",
        "identity tests must seed relative tasks/foo.json, not a bare basename"
    );

    let wt_parent = TempDir::new().unwrap();
    let wt_path = wt_parent.path().join("wt");
    assert!(
        std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feat/update-from-json-pin",
                wt_path.to_str().unwrap(),
            ])
            .current_dir(main.path())
            .status()
            .unwrap()
            .success(),
        "worktree add must succeed"
    );

    let wt_prd = wt_path.join("tasks/foo.json");
    assert!(wt_prd.is_file(), "worktree must have tasks/foo.json");

    let main_before = fs::read_to_string(&main_prd).unwrap();
    let marker = "upd-rel-prd-files-pin";
    let overlay = serde_json::json!({"id": "SEED-001", "notes": marker}).to_string();
    let result = update(main.path(), &overlay, Some(wt_prd.as_path()))
        .expect("relative prd_files + worktree --from-json must be registered for update");
    assert_eq!(result.task_id, "WT-SEED-001");
    assert_eq!(
        result.prd_path.as_ref().map(|p| p.canonicalize().unwrap()),
        Some(wt_prd.canonicalize().unwrap()),
        "write target must be the worktree flag path (never remapped away)"
    );

    // Re-open so we see the writer connection's commit (WAL).
    let conn = rusqlite::Connection::open(main.path().join("tasks.db")).unwrap();
    let notes: Option<String> = conn
        .query_row(
            "SELECT notes FROM tasks WHERE id = 'WT-SEED-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(notes.as_deref(), Some(marker));

    let main_after = fs::read_to_string(&main_prd).unwrap();
    assert_eq!(
        main_before, main_after,
        "main JSON must be unchanged when --from-json pins the worktree path"
    );
    let wt_after = fs::read_to_string(&wt_prd).unwrap();
    assert!(
        wt_after.contains(marker),
        "worktree JSON must receive the patch"
    );
}

// ---------------------------------------------------------------------------
// FEAT-006: refuse unpinned add when ≥2 non-NULL prefixes
// ---------------------------------------------------------------------------

#[test]
fn test_unpinned_multi_prefix_add_refuses_no_db_no_json() {
    let _env = EnvIsolation::new();
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"a","taskPrefix":"alpha","userStories":[{"id":"SEED-001","title":"a","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"b","taskPrefix":"beta","userStories":[{"id":"SEED-001","title":"b","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    init(
        dir.path(),
        &[&a],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("alpha".to_string()),
    )
    .unwrap();
    init(
        dir.path(),
        &[&b],
        false,
        true, // append
        false,
        false,
        PrefixMode::Explicit("beta".to_string()),
    )
    .unwrap();

    let before_a = fs::read_to_string(&a).unwrap();
    let before_b = fs::read_to_string(&b).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();

    let new_task = serde_json::json!({"id": "FIX-001", "title": "unpinned"}).to_string();
    let err = add(dir.path(), &new_task, None, &[], None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("--from-json"),
        "refuse must name --from-json: {msg}"
    );
    assert!(
        msg.contains("TASK_MGR_ACTIVE_PREFIX"),
        "refuse must name TASK_MGR_ACTIVE_PREFIX: {msg}"
    );

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "refused add must not insert a DB row");
    assert_eq!(
        fs::read_to_string(&a).unwrap(),
        before_a,
        "must not write PRD #1 JSON"
    );
    assert_eq!(
        fs::read_to_string(&b).unwrap(),
        before_b,
        "must not write PRD #2 JSON"
    );
}

#[test]
fn test_unpinned_multi_prefix_depended_on_by_refuses() {
    // Pin 1: --depended-on-by alone cannot select among ≥2 prefixes.
    let _env = EnvIsolation::new();
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"a","taskPrefix":"alpha","userStories":[{"id":"SEED-001","title":"a","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"b","taskPrefix":"beta","userStories":[{"id":"MILESTONE-1","title":"b","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    init(
        dir.path(),
        &[&a],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("alpha".to_string()),
    )
    .unwrap();
    init(
        dir.path(),
        &[&b],
        false,
        true,
        false,
        false,
        PrefixMode::Explicit("beta".to_string()),
    )
    .unwrap();

    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    let before_b = fs::read_to_string(&b).unwrap();

    let new_task = serde_json::json!({"id": "FIX-001", "title": "dep-only"}).to_string();
    let err = add(
        dir.path(),
        &new_task,
        None,
        &["beta-MILESTONE-1".to_string()],
        None,
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("--from-json"), "{msg}");
    assert!(msg.contains("TASK_MGR_ACTIVE_PREFIX"), "{msg}");

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "must not insert");
    assert_eq!(
        fs::read_to_string(&b).unwrap(),
        before_b,
        "must not sync JSON"
    );
}

#[test]
fn test_multi_prefix_from_json_pin_still_adds() {
    let _env = EnvIsolation::new();
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"a","taskPrefix":"alpha","userStories":[{"id":"SEED-001","title":"a","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"b","taskPrefix":"beta","userStories":[{"id":"SEED-001","title":"b","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    init(
        dir.path(),
        &[&a],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("alpha".to_string()),
    )
    .unwrap();
    init(
        dir.path(),
        &[&b],
        false,
        true,
        false,
        false,
        PrefixMode::Explicit("beta".to_string()),
    )
    .unwrap();

    let before_a = fs::read_to_string(&a).unwrap();
    let new_task = serde_json::json!({"id": "FIX-001", "title": "pinned"}).to_string();
    let result = add(dir.path(), &new_task, None, &[], Some(&b)).unwrap();
    assert_eq!(result.task_id, "beta-FIX-001");
    assert_eq!(
        result.prd_path.as_ref().map(|p| p.canonicalize().unwrap()),
        Some(b.canonicalize().unwrap()),
        "write target is the --from-json path"
    );
    assert_eq!(
        fs::read_to_string(&a).unwrap(),
        before_a,
        "unpinned PRD must stay untouched"
    );
    let updated: Value = serde_json::from_str(&fs::read_to_string(&b).unwrap()).unwrap();
    assert!(
        updated["userStories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_str() == Some("FIX-001")),
        "pinned PRD must gain unprefixed FIX-001"
    );
}

#[test]
fn test_multi_prefix_env_pin_still_adds() {
    let _env = EnvIsolation::new();
    let dir = TempDir::new().unwrap();
    let a = dir.path().join("a.json");
    let b = dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"a","taskPrefix":"alpha","userStories":[{"id":"SEED-001","title":"a","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"b","taskPrefix":"beta","userStories":[{"id":"SEED-001","title":"b","priority":10,"passes":false}]}"#,
    )
    .unwrap();
    init(
        dir.path(),
        &[&a],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("alpha".to_string()),
    )
    .unwrap();
    init(
        dir.path(),
        &[&b],
        false,
        true,
        false,
        false,
        PrefixMode::Explicit("beta".to_string()),
    )
    .unwrap();

    // EnvIsolation cleared the var under the mutex; set for this call.
    // Drop restores `prior` (None) when `_env` leaves scope.
    unsafe { std::env::set_var(ACTIVE_PREFIX_ENV, "beta") };
    let new_task = serde_json::json!({"id": "FIX-001", "title": "env-pinned"}).to_string();
    let result = add(dir.path(), &new_task, None, &[], None).unwrap();
    assert_eq!(result.task_id, "beta-FIX-001");
}
