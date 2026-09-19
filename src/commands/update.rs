//! Overlay whitelist + reject for `task-mgr update` (CONTRACT-002 / FEAT-003).
//!
//! Validator-only: parse overlay as `serde_json::Value` object, reject
//! lifecycle keys / unknown keys / type-null violations. Load-merge-write
//! (SQL + JSON patch) lands in FEAT-004 — do not add writers here.
//!
//! Full contract: `## CONTRACT-002` in `tasks/progress-a8855e28.txt`.

// Production caller lands in FEAT-004 (`update` load-merge-write); until then
// cargo check sees the API as unused (unit tests exercise it via --tests).
#![allow(dead_code)]

use serde_json::{Map, Value};

use crate::{TaskMgrError, TaskMgrResult};

/// JSON camelCase whitelist + lookup `id` (CONTRACT-002).
const OVERLAY_WHITELIST: &[&str] = &[
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

/// Present overlay value that may be JSON `null` (clear) or a typed value.
///
/// Distinguishes key-absent (`Option::None` on the field) from key-present-null
/// (`Some(Nullable::Null)`) for FEAT-004 merge.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Nullable<T> {
    Null,
    Present(T),
}

/// Validated overlay ready for FEAT-004 merge. Keys are JSON camelCase.
/// Presence means "apply"; absent means "leave unchanged".
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ValidatedUpdateOverlay {
    /// Lookup-only; never SET on DB / never copied onto JSON story id.
    pub id: String,
    pub title: Option<String>,
    pub description: Option<Nullable<String>>,
    pub notes: Option<Nullable<String>>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub touches_files: Option<Vec<String>>,
    pub depends_on: Option<Vec<String>>,
    /// From either `estimatedEffort` or `difficulty` (not both — rejected).
    pub difficulty: Option<Nullable<String>>,
    pub model: Option<Nullable<String>>,
    pub escalation_note: Option<Nullable<String>>,
    pub required_tests: Option<Vec<String>>,
    pub max_retries: Option<i32>,
    pub requires_human: Option<bool>,
    pub human_review_timeout: Option<Nullable<u32>>,
    pub claims_shared_infra: Option<Nullable<bool>>,
    pub review_scope: Option<Nullable<Value>>,
    pub severity: Option<Nullable<String>>,
    pub source_review: Option<Nullable<String>>,
    /// Object replaces; `Null` removes the JSON key (CONTRACT-003).
    pub human_review_outcome: Option<Nullable<Value>>,
}

/// Validate an update overlay `Value` (CONTRACT-002).
///
/// Order is load-bearing: object shape → top-level `status`/`passes` →
/// unknown keys (name all) → effort alias ambiguity → type/null table.
/// Never deserializes through `PrdUserStory`.
pub(crate) fn validate_update_overlay(overlay: &Value) -> TaskMgrResult<ValidatedUpdateOverlay> {
    let obj = overlay.as_object().ok_or_else(|| {
        TaskMgrError::invalid_state("update", "overlay", "JSON object", value_kind(overlay))
    })?;

    // Step 2: lifecycle keys (any value) before unknown-key / type tables.
    if obj.contains_key("status") || obj.contains_key("passes") {
        return Err(lifecycle_reject(obj));
    }

    // Step 3: unknown keys — name ALL present unknowns.
    let mut unknown: Vec<&str> = obj
        .keys()
        .filter(|k| k.as_str() != "id" && !OVERLAY_WHITELIST.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(TaskMgrError::invalid_state(
            "update",
            "overlay",
            "whitelist keys only (see `task-mgr update` docs)",
            format!("unknown keys: {}", unknown.join(", ")),
        ));
    }

    // Step 5 (presence check, any values): both effort aliases → ambiguous.
    // Checked before type/null so wrong-typed dual keys still get ambiguity.
    let has_effort = obj.contains_key("estimatedEffort");
    let has_difficulty = obj.contains_key("difficulty");
    if has_effort && has_difficulty {
        return Err(TaskMgrError::invalid_state(
            "update",
            "overlay",
            "exactly one of estimatedEffort or difficulty",
            "both estimatedEffort and difficulty present (ambiguous)",
        ));
    }

    // Step 4: type / null table (and required lookup id).
    let id = require_nonempty_string(obj, "id")?;

    let title = optional_nonempty_string(obj, "title")?;
    let description = optional_nullable_string(obj, "description")?;
    let notes = optional_nullable_string(obj, "notes")?;
    let model = optional_nullable_string(obj, "model")?;
    let escalation_note = optional_nullable_string(obj, "escalationNote")?;
    let severity = optional_nullable_string(obj, "severity")?;
    let source_review = optional_nullable_string(obj, "sourceReview")?;

    let difficulty = if has_effort {
        optional_nullable_string(obj, "estimatedEffort")?
    } else if has_difficulty {
        optional_nullable_string(obj, "difficulty")?
    } else {
        None
    };

    let acceptance_criteria = optional_string_array(obj, "acceptanceCriteria")?;
    let touches_files = optional_string_array(obj, "touchesFiles")?;
    let depends_on = optional_string_array(obj, "dependsOn")?;
    let required_tests = optional_string_array(obj, "requiredTests")?;

    let requires_human = optional_required_bool(obj, "requiresHuman")?;
    let max_retries = optional_required_i32(obj, "maxRetries")?;
    let human_review_timeout = optional_nullable_u32(obj, "humanReviewTimeout")?;
    let claims_shared_infra = optional_nullable_bool(obj, "claimsSharedInfra")?;
    let review_scope = optional_nullable_value(obj, "reviewScope")?;
    let human_review_outcome = optional_outcome(obj, "humanReviewOutcome")?;

    Ok(ValidatedUpdateOverlay {
        id,
        title,
        description,
        notes,
        acceptance_criteria,
        touches_files,
        depends_on,
        difficulty,
        model,
        escalation_note,
        required_tests,
        max_retries,
        requires_human,
        human_review_timeout,
        claims_shared_infra,
        review_scope,
        severity,
        source_review,
        human_review_outcome,
    })
}

fn lifecycle_reject(obj: &Map<String, Value>) -> TaskMgrError {
    let mut keys = Vec::new();
    if obj.contains_key("status") {
        keys.push("status");
    }
    if obj.contains_key("passes") {
        keys.push("passes");
    }
    TaskMgrError::invalid_state(
        "update",
        "overlay",
        "whitelist fields only — status/passes are lifecycle (use `task-mgr complete` / `fail` / `skip` / `<task-status>`)",
        format!("top-level {} present", keys.join(" and ")),
    )
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_err(key: &str, expected: &str, actual: &Value) -> TaskMgrError {
    TaskMgrError::invalid_state("update", key, expected, value_kind(actual))
}

fn require_nonempty_string(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<String> {
    match obj.get(key) {
        None => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "non-empty string (required lookup id)",
            "missing",
        )),
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "non-empty string",
            "empty string",
        )),
        Some(other) => Err(type_err(key, "non-empty string", other)),
    }
}

fn optional_nonempty_string(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<String>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "non-empty string",
            "empty string",
        )),
        Some(other) => Err(type_err(key, "non-empty string", other)),
    }
}

fn optional_nullable_string(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<String>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(Value::String(s)) => Ok(Some(Nullable::Present(s.clone()))),
        Some(other) => Err(type_err(key, "string or null", other)),
    }
}

fn optional_string_array(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Vec<String>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "array of strings ([] clears; null is illegal)",
            "null",
        )),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, elem) in arr.iter().enumerate() {
                match elem.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(TaskMgrError::invalid_state(
                            "update",
                            key,
                            "array of strings",
                            format!("element[{i}] is {}", value_kind(elem)),
                        ));
                    }
                }
            }
            Ok(Some(out))
        }
        Some(other) => Err(type_err(key, "array of strings", other)),
    }
}

/// `requiresHuman`: JSON bool only — null / non-bool reject (stricter than serde Option).
fn optional_required_bool(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<bool>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::Null) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "bool (null is illegal)",
            "null",
        )),
        Some(other) => Err(type_err(key, "bool", other)),
    }
}

/// `maxRetries`: JSON integer only — null rejects (column NOT NULL; do not default to 3).
fn optional_required_i32(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<i32>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Err(TaskMgrError::invalid_state(
            "update",
            key,
            "integer (null is illegal; column NOT NULL)",
            "null",
        )),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i32::try_from(i).map(Some).map_err(|_| {
                    TaskMgrError::invalid_state(
                        "update",
                        key,
                        "integer fitting i32",
                        format!("{i}"),
                    )
                })
            } else {
                Err(TaskMgrError::invalid_state(
                    "update",
                    key,
                    "integer",
                    "non-integer number",
                ))
            }
        }
        Some(other) => Err(type_err(key, "integer", other)),
    }
}

fn optional_nullable_u32(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<u32>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                u32::try_from(u)
                    .map(|v| Some(Nullable::Present(v)))
                    .map_err(|_| {
                        TaskMgrError::invalid_state(
                            "update",
                            key,
                            "unsigned integer fitting u32 or null",
                            format!("{u}"),
                        )
                    })
            } else if n.as_i64().is_some_and(|i| i < 0) {
                Err(TaskMgrError::invalid_state(
                    "update",
                    key,
                    "unsigned integer or null",
                    "negative integer",
                ))
            } else {
                Err(TaskMgrError::invalid_state(
                    "update",
                    key,
                    "unsigned integer or null",
                    "non-integer number",
                ))
            }
        }
        Some(other) => Err(type_err(key, "unsigned integer or null", other)),
    }
}

fn optional_nullable_bool(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<bool>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(Value::Bool(b)) => Ok(Some(Nullable::Present(*b))),
        Some(other) => Err(type_err(key, "bool or null", other)),
    }
}

/// `reviewScope`: any JSON value or null clears — do not `as_str()`.
fn optional_nullable_value(
    obj: &Map<String, Value>,
    key: &str,
) -> TaskMgrResult<Option<Nullable<Value>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(v) => Ok(Some(Nullable::Present(v.clone()))),
    }
}

/// `humanReviewOutcome`: object or null (null removes JSON key). Nested
/// `passes` inside the object is data, not a top-level lifecycle key.
fn optional_outcome(obj: &Map<String, Value>, key: &str) -> TaskMgrResult<Option<Nullable<Value>>> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(Nullable::Null)),
        Some(v) if v.is_object() => Ok(Some(Nullable::Present(v.clone()))),
        Some(other) => Err(type_err(key, "object or null", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err_string(overlay: Value) -> String {
        validate_update_overlay(&overlay)
            .expect_err("expected reject")
            .to_string()
    }

    fn assert_invalid_update(overlay: Value, needle: &str) {
        let msg = err_string(overlay);
        assert!(
            msg.contains("Invalid state for update"),
            "expected invalid_state(\"update\", …), got: {msg}"
        );
        assert!(
            msg.contains(needle),
            "expected message to contain {needle:?}, got: {msg}"
        );
    }

    #[test]
    fn rejects_non_object() {
        assert_invalid_update(json!([]), "JSON object");
        assert_invalid_update(json!("x"), "JSON object");
        assert_invalid_update(json!(1), "JSON object");
        assert_invalid_update(Value::Null, "JSON object");
        assert_invalid_update(json!(true), "JSON object");
    }

    #[test]
    fn lifecycle_status_or_passes_before_unknown_and_type() {
        // Full blob with passes:false + valid notes → lifecycle, not unknown/type.
        let msg = err_string(json!({
            "id": "FEAT-001",
            "passes": false,
            "notes": "ok",
            "priority": 1,
            "foo": true
        }));
        assert!(msg.contains("Invalid state for update"), "{msg}");
        assert!(
            msg.contains("complete") && msg.contains("fail") && msg.contains("skip"),
            "lifecycle pointer missing: {msg}"
        );
        assert!(msg.contains("<task-status>"), "{msg}");
        assert!(msg.contains("passes"), "{msg}");
        assert!(
            !msg.contains("unknown keys"),
            "must not fall through to unknown-key: {msg}"
        );

        // status:done alone
        assert_invalid_update(json!({"id": "FEAT-001", "status": "done"}), "status");

        // top-level passes: null alone
        assert_invalid_update(json!({"id": "FEAT-001", "passes": null}), "passes");
    }

    #[test]
    fn unknown_keys_name_all_no_partial_apply() {
        let msg = err_string(json!({
            "id": "FEAT-001",
            "notes": "keep",
            "priority": 1,
            "foo": true,
            "synergyWith": ["A"],
            "batchWith": ["B"],
            "conflictsWith": ["C"],
            "newId": "X"
        }));
        assert!(msg.contains("unknown keys"), "{msg}");
        for k in [
            "priority",
            "foo",
            "synergyWith",
            "batchWith",
            "conflictsWith",
            "newId",
        ] {
            assert!(msg.contains(k), "missing {k} in: {msg}");
        }
        assert!(
            !msg.contains("coming soon"),
            "priority must not be a coming-soon message: {msg}"
        );
    }

    #[test]
    fn both_effort_aliases_ambiguous() {
        assert_invalid_update(
            json!({
                "id": "FEAT-001",
                "estimatedEffort": "high",
                "difficulty": "low"
            }),
            "ambiguous",
        );
        // Any values — including nulls — still ambiguous.
        assert_invalid_update(
            json!({
                "id": "FEAT-001",
                "estimatedEffort": null,
                "difficulty": null
            }),
            "ambiguous",
        );
    }

    #[test]
    fn nested_passes_inside_human_review_outcome_allowed() {
        let v = validate_update_overlay(&json!({
            "id": "CLARIFY-001",
            "humanReviewOutcome": {
                "passes": false,
                "resolvedAt": "2026-09-19",
                "resolvedBy": "op"
            }
        }))
        .expect("nested passes must be accepted");
        match v.human_review_outcome {
            Some(Nullable::Present(Value::Object(m))) => {
                assert_eq!(m.get("passes"), Some(&json!(false)));
            }
            other => panic!("expected Present(object), got {other:?}"),
        }
    }

    #[test]
    fn id_only_succeeds() {
        let v = validate_update_overlay(&json!({"id": "FEAT-001"})).expect("id-only ok");
        assert_eq!(v.id, "FEAT-001");
        assert!(v.title.is_none());
        assert!(v.notes.is_none());
        assert!(v.human_review_outcome.is_none());
    }

    #[test]
    fn title_null_or_empty_rejects() {
        assert_invalid_update(json!({"id": "FEAT-001", "title": null}), "title");
        assert_invalid_update(json!({"id": "FEAT-001", "title": ""}), "empty string");
    }

    #[test]
    fn string_nullable_scalars_null_clear() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "notes": null,
            "description": null,
            "model": null
        }))
        .expect("null clears");
        assert_eq!(v.notes, Some(Nullable::Null));
        assert_eq!(v.description, Some(Nullable::Null));
        assert_eq!(v.model, Some(Nullable::Null));
    }

    #[test]
    fn string_arrays_empty_clears_null_errors() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "dependsOn": [],
            "touchesFiles": ["src/a.rs"]
        }))
        .expect("arrays ok");
        assert_eq!(v.depends_on.as_deref(), Some(&[][..]));
        assert_eq!(v.touches_files, Some(vec!["src/a.rs".to_string()]));

        assert_invalid_update(json!({"id": "FEAT-001", "dependsOn": null}), "dependsOn");
        assert_invalid_update(
            json!({"id": "FEAT-001", "acceptanceCriteria": [1]}),
            "acceptanceCriteria",
        );
    }

    #[test]
    fn requires_human_bool_only() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "requiresHuman": true
        }))
        .expect("bool ok");
        assert_eq!(v.requires_human, Some(true));

        assert_invalid_update(json!({"id": "FEAT-001", "requiresHuman": null}), "null");
        assert_invalid_update(json!({"id": "FEAT-001", "requiresHuman": 1}), "bool");
        assert_invalid_update(json!({"id": "FEAT-001", "requiresHuman": "true"}), "bool");
    }

    #[test]
    fn max_retries_integer_null_rejects() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "maxRetries": 5
        }))
        .expect("int ok");
        assert_eq!(v.max_retries, Some(5));

        assert_invalid_update(json!({"id": "FEAT-001", "maxRetries": null}), "null");
        assert_invalid_update(json!({"id": "FEAT-001", "maxRetries": 1.5}), "integer");
    }

    #[test]
    fn claims_shared_infra_bool_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "claimsSharedInfra": null
        }))
        .expect("null clears");
        assert_eq!(v.claims_shared_infra, Some(Nullable::Null));

        assert_invalid_update(
            json!({"id": "FEAT-001", "claimsSharedInfra": "true"}),
            "claimsSharedInfra",
        );
    }

    #[test]
    fn human_review_timeout_unsigned_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "humanReviewTimeout": 60
        }))
        .expect("u32 ok");
        assert_eq!(v.human_review_timeout, Some(Nullable::Present(60)));

        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewTimeout": -1}),
            "humanReviewTimeout",
        );
        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewTimeout": 1.5}),
            "humanReviewTimeout",
        );
    }

    #[test]
    fn review_scope_any_json_value_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "reviewScope": {"paths": ["src/"]}
        }))
        .expect("object ok");
        assert!(matches!(v.review_scope, Some(Nullable::Present(_))));

        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "reviewScope": ["a", 1, true]
        }))
        .expect("array ok — do not as_str()");
        assert!(matches!(
            v.review_scope,
            Some(Nullable::Present(Value::Array(_)))
        ));

        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "reviewScope": null
        }))
        .expect("null clears");
        assert_eq!(v.review_scope, Some(Nullable::Null));
    }

    #[test]
    fn human_review_outcome_object_or_null() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "humanReviewOutcome": null
        }))
        .expect("null removes key");
        assert_eq!(v.human_review_outcome, Some(Nullable::Null));

        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewOutcome": []}),
            "humanReviewOutcome",
        );
        assert_invalid_update(
            json!({"id": "FEAT-001", "humanReviewOutcome": "x"}),
            "object or null",
        );
    }

    #[test]
    fn effort_alias_either_key_alone_ok() {
        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "estimatedEffort": "high"
        }))
        .expect("estimatedEffort alone");
        assert_eq!(v.difficulty, Some(Nullable::Present("high".into())));

        let v = validate_update_overlay(&json!({
            "id": "FEAT-001",
            "difficulty": null
        }))
        .expect("difficulty null clears");
        assert_eq!(v.difficulty, Some(Nullable::Null));
    }

    #[test]
    fn missing_id_rejects() {
        assert_invalid_update(json!({"notes": "x"}), "id");
    }

    /// Known-bad discriminator: typed PrdUserStory parse would hide rejects.
    #[test]
    fn must_not_use_prd_user_story_deserialize() {
        // If someone switched to from_value::<PrdUserStory>, passes:false would
        // deserialize (default/ignored) and priority would be silently accepted.
        // This overlay must still take the lifecycle reject.
        let overlay = json!({
            "id": "FEAT-001",
            "title": "t",
            "priority": 5,
            "passes": false
        });
        let msg = err_string(overlay);
        assert!(msg.contains("passes") || msg.contains("status"), "{msg}");
        assert!(msg.contains("complete"), "{msg}");
    }
}
