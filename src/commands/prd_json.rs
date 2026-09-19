//! Shared PRD JSON write chokepoint: unique tmp + rename, and append.
//!
//! Pin 12 / PR-1 write chokepoint: one process-local `unique_tmp_path` (pid +
//! counter + nanos) shared by `append_user_story` and `prd_reconcile` writers.
//! Existing stories are Value-mutated only — never deserialized through
//! `PrdUserStory` (unknown keys must survive). This module must not import
//! `commands::add` or `commands::update`.
//!
//! **PR-2 CONTRACT-001:** `patch_user_story` (FEAT-002) is the update JSON
//! merge chokepoint — skip overlay `id`, write `dependsOn` unprefixed via
//! `strip_prefix_in_id_array`, forward `command: &str` into every
//! `invalid_state`. Full contract: `## CONTRACT-001` in
//! `tasks/progress-a8855e28.txt` (see also `src/commands/CLAUDE.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::commands::init::PrdUserStory;
use crate::loop_engine::output_parsing::strip_task_prefix;
use crate::output::ui;
use crate::{TaskMgrError, TaskMgrResult};

/// Build a per-writer tmp path next to `prd_path` for atomic rename.
///
/// Scheme: `.{base}.{pid}-{n}-{nanos}.tmp` in the same directory as the target
/// (learning #2667 / #4564: same-dir rename; never stage under `/tmp`).
///
/// Infallible. Side effect: increments the shared process-local COUNTER only.
/// Successive calls in the same process MUST yield distinct PathBufs.
pub(crate) fn unique_tmp_path(prd_path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let parent = prd_path.parent().unwrap_or_else(|| Path::new("."));
    let base = prd_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "prd.json".to_string());
    parent.join(format!(".{base}.{pid}-{n}-{nanos}.tmp"))
}

/// Strip the active prefix from every string element of the `key` array on a
/// serialized userStory object, rewriting the DB-prefixed relationship ids into
/// the unprefixed JSON convention. No-op when the key is absent or not an array.
pub(crate) fn strip_prefix_in_id_array(
    obj: &mut serde_json::Map<String, Value>,
    key: &str,
    prefix: Option<&str>,
) {
    let Some(arr) = obj.get_mut(key).and_then(|v| v.as_array_mut()) else {
        return;
    };
    for elem in arr.iter_mut() {
        if let Some(id) = elem.as_str() {
            *elem = Value::String(strip_task_prefix(id, prefix).to_string());
        }
    }
}

/// Overlay keys that `patch_user_story` may merge onto an existing story.
/// Lookup-only `id` is intentionally absent — merge skips it.
const PATCH_WHITELIST: &[&str] = &[
    "title",
    "description",
    "notes",
    "acceptanceCriteria",
    "touchesFiles",
    "dependsOn",
    "estimatedEffort",
    "difficulty",
    "model",
    "escalationNote",
    "requiredTests",
    "maxRetries",
    "requiresHuman",
    "humanReviewTimeout",
    "claimsSharedInfra",
    "reviewScope",
    "severity",
    "sourceReview",
    "humanReviewOutcome",
];

/// Append one user story to the PRD JSON's `userStories` array, atomically.
///
/// Behavior-preserving move of today's `add::append_task_to_prd_json`. The file
/// is Value-parsed and written back; only `story` is typed via `to_value`.
/// Existing entries are mutated in place as Value objects (unknown keys survive).
///
/// `prefix` must be the active PRD `task_prefix` (or `None`). Callers with an
/// empty / NULL-prefix pin MUST pass `None` — never an id-shape segment such
/// as `CODE` from `CODE-FIX-001` (that would write `FIX-001` into JSON).
pub(crate) fn append_user_story(
    prd_path: &Path,
    story: &PrdUserStory,
    depended_on_by: &[String],
    prefix: Option<&str>,
) -> TaskMgrResult<()> {
    // The DB stores prefixed ids (`e474b6f2-CODE-REVIEW-2`) but PRD task-list
    // JSON stores them unprefixed (`CODE-REVIEW-2`); the importer re-applies the
    // prefix idempotently. Mirror `prd_reconcile::update_prd_task_passes` and
    // write/match ids in the JSON's unprefixed convention so the new entry stays
    // consistent with its siblings and the reverse-link sync actually lands.
    let base_story_id = strip_task_prefix(&story.id, prefix);
    let original = fs::read_to_string(prd_path).map_err(|e| {
        TaskMgrError::invalid_state(
            "add",
            "prd file",
            "readable",
            format!("{}: {}", prd_path.display(), e),
        )
    })?;

    let mut root: Value = serde_json::from_str(&original).map_err(|e| {
        TaskMgrError::invalid_state(
            "add",
            "prd json",
            "valid JSON object",
            format!("{}: {}", prd_path.display(), e),
        )
    })?;

    let root_obj = root.as_object_mut().ok_or_else(|| {
        TaskMgrError::invalid_state("add", "prd json", "JSON object at root", "not an object")
    })?;

    // Reject duplicate IDs already present in the file (defence-in-depth —
    // DB check would have caught this too, unless someone hand-edited the
    // JSON out-of-band).
    if let Some(user_stories) = root_obj.get("userStories").and_then(|v| v.as_array()) {
        let dup = user_stories.iter().any(|t| {
            t.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == story.id || id == base_story_id)
        });
        if dup {
            return Err(TaskMgrError::invalid_state(
                "add",
                "task id",
                "not already present in PRD JSON",
                format!("{} already in {}", story.id, prd_path.display()),
            ));
        }
    }

    // Serialize the new entry, then rewrite its id-bearing fields into the
    // unprefixed JSON convention (the struct carries the prefixed DB forms).
    let mut task_value = serde_json::to_value(story)?;
    if let Some(obj) = task_value.as_object_mut() {
        obj.insert("id".to_string(), Value::String(base_story_id.to_string()));
        for key in ["dependsOn", "synergyWith", "batchWith", "conflictsWith"] {
            strip_prefix_in_id_array(obj, key, prefix);
        }
    }

    let arr = root_obj
        .entry("userStories")
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = arr.as_array_mut().ok_or_else(|| {
        TaskMgrError::invalid_state(
            "add",
            "userStories",
            "JSON array",
            "present but not an array",
        )
    })?;

    // Reverse-link updates: for each requested existing task id, find its
    // entry and push story.id into its dependsOn array (creating if missing).
    // Missing targets log a warning but don't fail — DB is the source of truth.
    for existing_id in depended_on_by {
        // The JSON may carry either convention (originally-authored entries are
        // unprefixed; entries spawned by an older `add` may be prefixed), so
        // match on both the prefixed target and its unprefixed base.
        let base_target = strip_task_prefix(existing_id, prefix);
        let mut matched = false;
        for entry in arr.iter_mut() {
            let Some(obj) = entry.as_object_mut() else {
                continue;
            };
            let is_match = obj
                .get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == existing_id || id == base_target);
            if !is_match {
                continue;
            }
            matched = true;
            let deps_entry = obj
                .entry("dependsOn".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let deps_arr = deps_entry.as_array_mut().ok_or_else(|| {
                TaskMgrError::invalid_state(
                    "add",
                    "dependsOn",
                    "JSON array",
                    format!("{} dependsOn present but not an array", existing_id),
                )
            })?;
            // Push the unprefixed new id; dedup against both forms in case the
            // array already mixes conventions.
            let already = deps_arr.iter().any(|v| {
                v.as_str()
                    .is_some_and(|s| s == story.id || s == base_story_id)
            });
            if !already {
                deps_arr.push(Value::String(base_story_id.to_string()));
            }
            break;
        }
        if !matched {
            ui::emit_err(&format!(
                "Warning: --depended-on-by target {} not found in PRD JSON {}; DB updated but JSON dependsOn not synced for that target",
                existing_id,
                prd_path.display(),
            ));
        }
    }

    arr.push(task_value);

    let pretty = serde_json::to_string_pretty(&root)?;
    // Preserve trailing newline if original had one.
    let output = if original.ends_with('\n') {
        format!("{}\n", pretty)
    } else {
        pretty
    };

    atomic_write(prd_path, &output, "add")?;
    Ok(())
}

/// Patch one existing `userStories[]` entry by Value-merge (CONTRACT-001).
///
/// Locates the story by prefixed or unprefixed `story_id` (same as
/// [`append_user_story`]), merges whitelist keys from `overlay` onto the
/// existing Value object (never `from_value::<PrdUserStory>`), and writes via
/// [`atomic_write`] with `command` forwarded into every `invalid_state`.
///
/// Merge rules:
/// - Overlay `id` is skipped — the JSON story's `id` stays byte-identical.
/// - `humanReviewOutcome: null` removes the key; a present object replaces.
/// - Either `difficulty` or `estimatedEffort` writes canonical
///   `estimatedEffort` and removes leftover `difficulty` (both present is a
///   caller/validator reject — not handled here).
/// - `dependsOn` is rewritten unprefixed via [`strip_prefix_in_id_array`].
/// - Extra keys already on the story survive.
pub(crate) fn patch_user_story(
    prd_path: &Path,
    story_id: &str,
    overlay: &Value,
    prefix: Option<&str>,
    command: &str,
) -> TaskMgrResult<()> {
    let base_story_id = strip_task_prefix(story_id, prefix);
    let original = fs::read_to_string(prd_path).map_err(|e| {
        TaskMgrError::invalid_state(
            command,
            "prd file",
            "readable",
            format!("{}: {}", prd_path.display(), e),
        )
    })?;

    let mut root: Value = serde_json::from_str(&original).map_err(|e| {
        TaskMgrError::invalid_state(
            command,
            "prd json",
            "valid JSON object",
            format!("{}: {}", prd_path.display(), e),
        )
    })?;

    let root_obj = root.as_object_mut().ok_or_else(|| {
        TaskMgrError::invalid_state(command, "prd json", "JSON object at root", "not an object")
    })?;

    let arr = root_obj
        .get_mut("userStories")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| {
            TaskMgrError::invalid_state(
                command,
                "userStories",
                "JSON array",
                "missing or not an array",
            )
        })?;

    let mut matched = false;
    for entry in arr.iter_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let is_match = obj
            .get("id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| id == story_id || id == base_story_id);
        if !is_match {
            continue;
        }
        matched = true;

        let overlay_obj = overlay.as_object().ok_or_else(|| {
            TaskMgrError::invalid_state(command, "overlay", "JSON object", "not an object")
        })?;

        // Effort alias: either overlay key → canonical estimatedEffort; drop leftover difficulty.
        let effort_value = overlay_obj
            .get("estimatedEffort")
            .or_else(|| overlay_obj.get("difficulty"))
            .cloned();

        for key in PATCH_WHITELIST {
            if *key == "estimatedEffort" || *key == "difficulty" {
                continue;
            }
            let Some(value) = overlay_obj.get(*key) else {
                continue;
            };
            if *key == "humanReviewOutcome" && value.is_null() {
                obj.remove("humanReviewOutcome");
                continue;
            }
            obj.insert((*key).to_string(), value.clone());
        }

        if let Some(effort) = effort_value {
            obj.insert("estimatedEffort".to_string(), effort);
            obj.remove("difficulty");
        }

        if overlay_obj.contains_key("dependsOn") {
            strip_prefix_in_id_array(obj, "dependsOn", prefix);
        }
        break;
    }

    if !matched {
        return Err(TaskMgrError::invalid_state(
            command,
            "userStory",
            format!("story id {story_id} present in {}", prd_path.display()),
            "not found",
        ));
    }

    let pretty = serde_json::to_string_pretty(&root)?;
    let output = if original.ends_with('\n') {
        format!("{}\n", pretty)
    } else {
        pretty
    };

    atomic_write(prd_path, &output, command)?;
    Ok(())
}

/// Write `content` to `target` atomically via [`unique_tmp_path`] + rename.
///
/// `command` is forwarded into every `invalid_state` so update/patch errors
/// cannot say `"add"`. [`append_user_story`] keeps passing `"add"`.
fn atomic_write(target: &Path, content: &str, command: &str) -> TaskMgrResult<()> {
    let tmp_path = unique_tmp_path(target);
    fs::write(&tmp_path, content).map_err(|e| {
        TaskMgrError::invalid_state(
            command,
            "prd file write",
            "successful tmp write",
            format!("{}: {}", tmp_path.display(), e),
        )
    })?;
    fs::rename(&tmp_path, target).map_err(|e| {
        // Best-effort cleanup.
        let _ = fs::remove_file(&tmp_path);
        TaskMgrError::invalid_state(
            command,
            "prd file rename",
            "successful rename",
            format!("{} -> {}: {}", tmp_path.display(), target.display(), e),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn minimal_story(id: &str, title: &str, priority: i32) -> PrdUserStory {
        PrdUserStory {
            id: id.to_string(),
            title: title.to_string(),
            description: None,
            priority,
            passes: false,
            notes: None,
            acceptance_criteria: vec![],
            review_scope: None,
            severity: None,
            source_review: None,
            touches_files: vec![],
            depends_on: vec![],
            synergy_with: vec![],
            batch_with: vec![],
            conflicts_with: vec![],
            model: None,
            difficulty: None,
            escalation_note: None,
            required_tests: vec![],
            max_retries: None,
            requires_human: None,
            human_review_timeout: None,
            claims_shared_infra: None,
            human_review_outcome: None,
        }
    }

    #[test]
    fn test_unique_tmp_path_distinct_per_call() {
        // Path math only — no filesystem I/O (learning #2310).
        let prd = Path::new("/tmp/tasks/prd.json");
        let a = unique_tmp_path(prd);
        let b = unique_tmp_path(prd);
        assert_ne!(a, b, "successive calls must yield distinct tmp paths");
        let a_name = a.file_name().unwrap().to_string_lossy();
        assert!(a_name.starts_with(".prd.json."));
        assert!(a_name.ends_with(".tmp"));
        assert_eq!(a.parent(), Some(Path::new("/tmp/tasks")));
    }

    #[test]
    fn test_append_vs_passes_tmp_paths_do_not_collide() {
        // Structural collision defense: append_user_story and
        // update_prd_task_passes both call this same helper (shared AtomicU64).
        let prd = Path::new("tasks/demo.json");
        let append_tmp = unique_tmp_path(prd);
        let passes_tmp = unique_tmp_path(prd);
        assert_ne!(
            append_tmp, passes_tmp,
            "append vs update_prd_task_passes tmp names must not collide"
        );
        for p in [&append_tmp, &passes_tmp] {
            let name = p.file_name().unwrap().to_string_lossy();
            assert!(name.starts_with(".demo.json."));
            assert!(name.ends_with(".tmp"));
            assert!(!name.contains("task-mgr-add"));
        }
    }

    #[test]
    fn test_append_user_story_adds_to_userstories() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial = r#"{
  "project": "demo",
  "userStories": [
    {"id": "SEED-001", "title": "seed", "priority": 50, "passes": false}
  ]
}
"#;
        {
            let mut f = tmp.reopen().unwrap();
            f.write_all(initial.as_bytes()).unwrap();
        }

        let story = minimal_story("NEW-001", "new", 5);
        append_user_story(tmp.path(), &story, &[], None).unwrap();

        let after = fs::read_to_string(tmp.path()).unwrap();
        let v: Value = serde_json::from_str(&after).unwrap();
        let arr = v
            .get("userStories")
            .and_then(|v| v.as_array())
            .expect("userStories array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1].get("id").and_then(|v| v.as_str()), Some("NEW-001"));
        assert_eq!(arr[1].get("priority").and_then(|v| v.as_i64()), Some(5));
        assert!(after.ends_with('\n'), "trailing newline preserved");
    }

    #[test]
    fn test_append_rejects_duplicate_id_in_prd_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial =
            r#"{"userStories":[{"id":"DUP-001","title":"x","priority":50,"passes":false}]}"#;
        {
            let mut f = tmp.reopen().unwrap();
            f.write_all(initial.as_bytes()).unwrap();
        }
        let story = minimal_story("DUP-001", "again", 5);
        let err = append_user_story(tmp.path(), &story, &[], None).unwrap_err();
        assert!(format!("{err}").contains("DUP-001"));
    }

    #[test]
    fn test_append_preserves_unknown_keys_on_existing_stories() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial = r#"{
  "project": "demo",
  "userStories": [
    {
      "id": "SEED-001",
      "title": "seed",
      "priority": 50,
      "passes": false,
      "humanReviewOutcome": {
        "resolvedAt": "2026-01-01",
        "resolvedBy": "operator"
      }
    }
  ]
}
"#;
        {
            let mut f = tmp.reopen().unwrap();
            f.write_all(initial.as_bytes()).unwrap();
        }

        let story = minimal_story("NEW-001", "new", 5);
        append_user_story(tmp.path(), &story, &[], None).unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(tmp.path()).unwrap()).unwrap();
        let seed = after["userStories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"].as_str() == Some("SEED-001"))
            .expect("seed story preserved");
        assert!(
            seed.get("humanReviewOutcome").is_some(),
            "unknown key on existing story must survive append; got: {seed}"
        );
        assert_eq!(
            seed["humanReviewOutcome"]["resolvedBy"].as_str(),
            Some("operator")
        );
    }

    #[test]
    fn test_append_reverse_depended_on_by_and_prefix_strip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let initial = r#"{
  "userStories": [
    {"id": "MILESTONE-1", "title": "ms", "priority": 10, "passes": false}
  ]
}"#;
        {
            let mut f = tmp.reopen().unwrap();
            f.write_all(initial.as_bytes()).unwrap();
        }

        // Story carries prefixed DB forms; JSON must store unprefixed.
        let mut story = minimal_story("e474b6f2-CODE-FIX-001", "fix", 5);
        story.depends_on = vec!["e474b6f2-FEAT-1".to_string()];

        append_user_story(
            tmp.path(),
            &story,
            &["e474b6f2-MILESTONE-1".to_string()],
            Some("e474b6f2"),
        )
        .unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(tmp.path()).unwrap()).unwrap();
        let stories = after["userStories"].as_array().unwrap();

        let milestone = stories
            .iter()
            .find(|s| s["id"].as_str() == Some("MILESTONE-1"))
            .expect("milestone");
        let deps = milestone["dependsOn"]
            .as_array()
            .expect("dependsOn created");
        assert!(
            deps.iter().any(|v| v.as_str() == Some("CODE-FIX-001")),
            "reverse --depended-on-by must push unprefixed new id; got {deps:?}"
        );

        let new = stories
            .iter()
            .find(|s| s["id"].as_str() == Some("CODE-FIX-001"))
            .expect("new story with stripped id");
        let new_deps = new["dependsOn"].as_array().unwrap();
        assert!(
            new_deps.iter().any(|v| v.as_str() == Some("FEAT-1")),
            "dependsOn on new story must be prefix-stripped; got {new_deps:?}"
        );
        assert!(
            !new_deps
                .iter()
                .any(|v| v.as_str() == Some("e474b6f2-FEAT-1")),
            "prefixed form must not remain on new story dependsOn"
        );
    }

    fn write_prd(tmp: &tempfile::NamedTempFile, content: &str) {
        let mut f = tmp.reopen().unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn read_story(prd_path: &Path, id: &str) -> Value {
        let after: Value = serde_json::from_str(&fs::read_to_string(prd_path).unwrap()).unwrap();
        after["userStories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"].as_str() == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("story {id} not found"))
    }

    fn assert_invalid_state_command(err: &TaskMgrError, expected_command: &str) {
        match err {
            TaskMgrError::InvalidState { resource_type, .. } => {
                assert_eq!(
                    resource_type, expected_command,
                    "invalid_state command must be {expected_command:?}; got {resource_type:?}"
                );
            }
            other => panic!("expected InvalidState, got {other:?}"),
        }
    }

    #[test]
    fn test_patch_notes_preserves_extra_keys_and_id() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp,
            r#"{
  "userStories": [
    {
      "id": "FEAT-001",
      "title": "seed",
      "priority": 50,
      "passes": false,
      "customAgentKey": "keep-me",
      "notes": "old"
    }
  ]
}
"#,
        );

        let overlay = serde_json::json!({
            "id": "SHOULD-NOT-APPLY",
            "notes": "new notes"
        });
        patch_user_story(tmp.path(), "FEAT-001", &overlay, None, "update").unwrap();

        let entry = read_story(tmp.path(), "FEAT-001");
        assert_eq!(entry["id"].as_str(), Some("FEAT-001"));
        assert_eq!(entry["notes"].as_str(), Some("new notes"));
        assert_eq!(entry["title"].as_str(), Some("seed"));
        assert_eq!(entry["priority"].as_i64(), Some(50));
        assert_eq!(entry["customAgentKey"].as_str(), Some("keep-me"));
        let after = fs::read_to_string(tmp.path()).unwrap();
        assert!(after.ends_with('\n'), "trailing newline preserved");
    }

    #[test]
    fn test_patch_skips_overlay_id_prefixed_and_unprefixed_seed() {
        // Prefixed seed + prefixed lookup (append-style match on full id).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp,
            r#"{"userStories":[{"id":"a8855e28-FEAT-002","title":"seed","priority":1,"passes":false}]}"#,
        );

        let overlay = serde_json::json!({
            "id": "a8855e28-OTHER",
            "notes": "patched"
        });
        patch_user_story(
            tmp.path(),
            "a8855e28-FEAT-002",
            &overlay,
            Some("a8855e28"),
            "update",
        )
        .unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(tmp.path()).unwrap()).unwrap();
        let entry = &after["userStories"][0];
        assert_eq!(
            entry["id"].as_str(),
            Some("a8855e28-FEAT-002"),
            "story id must stay byte-identical to the seeded value"
        );
        assert_eq!(entry["notes"].as_str(), Some("patched"));

        // Unprefixed seed + prefixed lookup (append-style match on stripped base).
        let tmp2 = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp2,
            r#"{"userStories":[{"id":"FEAT-003","title":"seed","priority":1,"passes":false}]}"#,
        );
        let overlay2 = serde_json::json!({"id": "a8855e28-FEAT-003", "notes": "n2"});
        patch_user_story(
            tmp2.path(),
            "a8855e28-FEAT-003",
            &overlay2,
            Some("a8855e28"),
            "update",
        )
        .unwrap();
        let entry2 = read_story(tmp2.path(), "FEAT-003");
        assert_eq!(entry2["id"].as_str(), Some("FEAT-003"));
        assert_eq!(entry2["notes"].as_str(), Some("n2"));
    }

    #[test]
    fn test_patch_depends_on_written_unprefixed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp,
            r#"{"userStories":[{"id":"FEAT-010","title":"seed","priority":1,"passes":false}]}"#,
        );

        let overlay = serde_json::json!({
            "id": "FEAT-010",
            "dependsOn": ["a8855e28-CONTRACT-001", "a8855e28-FEAT-001"]
        });
        patch_user_story(
            tmp.path(),
            "a8855e28-FEAT-010",
            &overlay,
            Some("a8855e28"),
            "update",
        )
        .unwrap();

        let entry = read_story(tmp.path(), "FEAT-010");
        let deps = entry["dependsOn"].as_array().expect("dependsOn array");
        assert_eq!(
            deps,
            &vec![
                Value::String("CONTRACT-001".into()),
                Value::String("FEAT-001".into())
            ]
        );
        assert!(
            deps.iter()
                .all(|v| !v.as_str().unwrap_or("").starts_with("a8855e28-")),
            "prefixed forms must not remain; got {deps:?}"
        );
    }

    #[test]
    fn test_patch_human_review_outcome_null_removes_object_replaces() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp,
            r#"{
  "userStories": [
    {
      "id": "CLARIFY-001",
      "title": "seed",
      "priority": 1,
      "passes": false,
      "humanReviewOutcome": {"resolvedAt": "2026-01-01", "resolvedBy": "old"}
    }
  ]
}
"#,
        );

        let replace = serde_json::json!({
            "id": "CLARIFY-001",
            "humanReviewOutcome": {
                "resolvedAt": "2026-09-19",
                "resolvedBy": "operator",
                "confirmedValues": {"floor": 2}
            }
        });
        patch_user_story(tmp.path(), "CLARIFY-001", &replace, None, "update").unwrap();
        let entry = read_story(tmp.path(), "CLARIFY-001");
        assert_eq!(
            entry["humanReviewOutcome"]["resolvedBy"].as_str(),
            Some("operator")
        );
        assert_eq!(
            entry["humanReviewOutcome"]["confirmedValues"]["floor"].as_i64(),
            Some(2)
        );

        let remove = serde_json::json!({
            "id": "CLARIFY-001",
            "humanReviewOutcome": null
        });
        patch_user_story(tmp.path(), "CLARIFY-001", &remove, None, "update").unwrap();
        let entry = read_story(tmp.path(), "CLARIFY-001");
        assert!(
            entry.get("humanReviewOutcome").is_none(),
            "null must remove the key; got {entry}"
        );
    }

    #[test]
    fn test_patch_effort_alias_writes_estimated_effort_removes_difficulty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp,
            r#"{"userStories":[{"id":"FEAT-020","title":"seed","priority":1,"passes":false,"difficulty":"low"}]}"#,
        );

        let overlay = serde_json::json!({
            "id": "FEAT-020",
            "estimatedEffort": "high"
        });
        patch_user_story(tmp.path(), "FEAT-020", &overlay, None, "update").unwrap();
        let entry = read_story(tmp.path(), "FEAT-020");
        assert_eq!(entry["estimatedEffort"].as_str(), Some("high"));
        assert!(
            entry.get("difficulty").is_none(),
            "leftover difficulty must be removed; got {entry}"
        );

        // difficulty-only overlay also canonicalizes.
        let tmp2 = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp2,
            r#"{"userStories":[{"id":"FEAT-021","title":"seed","priority":1,"passes":false,"difficulty":"low"}]}"#,
        );
        let overlay2 = serde_json::json!({"id": "FEAT-021", "difficulty": "high"});
        patch_user_story(tmp2.path(), "FEAT-021", &overlay2, None, "update").unwrap();
        let entry2 = read_story(tmp2.path(), "FEAT-021");
        assert_eq!(entry2["estimatedEffort"].as_str(), Some("high"));
        assert!(entry2.get("difficulty").is_none());
    }

    #[test]
    fn test_patch_story_not_found_uses_update_command() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_prd(
            &tmp,
            r#"{"userStories":[{"id":"FEAT-001","title":"seed","priority":1,"passes":false}]}"#,
        );

        let overlay = serde_json::json!({"id": "MISSING-001", "notes": "x"});
        let err =
            patch_user_story(tmp.path(), "MISSING-001", &overlay, None, "update").unwrap_err();
        assert_invalid_state_command(&err, "update");
        assert!(format!("{err}").contains("MISSING-001") || format!("{err}").contains("not found"));
    }

    #[test]
    fn test_patch_unreadable_path_invalid_state_command_is_update() {
        let missing = std::env::temp_dir().join(format!(
            "task-mgr-patch-missing-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let overlay = serde_json::json!({"id": "FEAT-001", "notes": "x"});
        let err = patch_user_story(&missing, "FEAT-001", &overlay, None, "update").unwrap_err();
        assert_invalid_state_command(&err, "update");
    }
}
