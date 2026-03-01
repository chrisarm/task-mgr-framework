//! Curate subcommand implementations.
//!
//! Provides `curate retire` and `curate unretire` commands for managing
//! the institutional memory quality via soft-archiving stale learnings.

pub mod enrich;
pub mod output;
pub mod types;

pub use output::{format_enrich_text, format_retire_text, format_unretire_text};
pub use types::{
    EnrichCandidate, EnrichParams, EnrichResult, MergeClusterParams, MergeClusterResult,
    RetireParams, RetireResult, RetirementCandidate, UnretireResult,
};

use rusqlite::Connection;

use crate::TaskMgrResult;

/// Identifies retirement candidates and optionally soft-archives them.
///
/// Three criteria (OR'd together):
/// 1. age >= min_age_days AND confidence = 'low' AND times_applied = 0
/// 2. times_shown >= min_shows AND times_applied = 0
/// 3. times_shown >= min_shows*2 AND (times_applied/times_shown) < max_rate
///
/// Already-retired learnings (retired_at IS NOT NULL) are excluded.
pub fn curate_retire(conn: &Connection, params: RetireParams) -> TaskMgrResult<RetireResult> {
    let min_shows_doubled = i64::from(params.min_shows) * 2;

    let sql = "
        SELECT id, title, confidence,
               julianday('now') - julianday(created_at) AS age_days,
               times_shown, times_applied
        FROM learnings
        WHERE retired_at IS NULL
          AND (
            (julianday('now') - julianday(created_at) >= ?1
             AND confidence = 'low'
             AND times_applied = 0)
            OR
            (times_shown >= ?2 AND times_applied = 0)
            OR
            (times_shown >= ?3
             AND CAST(times_applied AS REAL) / CAST(times_shown AS REAL) < ?4)
          )
    ";

    let mut stmt = conn.prepare(sql)?;
    let candidates: Vec<RetirementCandidate> = stmt
        .query_map(
            rusqlite::params![
                i64::from(params.min_age_days),
                i64::from(params.min_shows),
                min_shows_doubled,
                params.max_rate
            ],
            |row| {
                let id: i64 = row.get("id")?;
                let title: String = row.get("title")?;
                let confidence: String = row.get("confidence")?;
                let age_days: f64 = row.get("age_days")?;
                let times_shown: i64 = row.get("times_shown")?;
                let times_applied: i64 = row.get("times_applied")?;

                let reason = build_reason(
                    &confidence,
                    age_days,
                    times_shown,
                    times_applied,
                    i64::from(params.min_age_days),
                    i64::from(params.min_shows),
                    min_shows_doubled,
                    params.max_rate,
                );

                Ok(RetirementCandidate { id, title, reason })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;

    let candidates_found = candidates.len();

    let learnings_retired = if params.dry_run {
        0
    } else {
        // Retire all candidates in a single transaction
        let ids: Vec<i64> = candidates.iter().map(|c| c.id).collect();
        retire_candidates(conn, &ids)?
    };

    Ok(RetireResult {
        dry_run: params.dry_run,
        candidates_found,
        learnings_retired,
        candidates,
    })
}

/// Determines which criterion matched and returns a human-readable reason string.
#[allow(clippy::too_many_arguments)]
fn build_reason(
    confidence: &str,
    age_days: f64,
    times_shown: i64,
    times_applied: i64,
    min_age_days: i64,
    min_shows: i64,
    min_shows_doubled: i64,
    max_rate: f64,
) -> String {
    let c1 = age_days >= min_age_days as f64 && confidence == "low" && times_applied == 0;
    let c2 = times_shown >= min_shows && times_applied == 0;
    let c3 =
        times_shown >= min_shows_doubled && (times_applied as f64 / times_shown as f64) < max_rate;

    match (c1, c2, c3) {
        (true, false, false) => format!(
            "Low-confidence learning not applied in {age_days:.0} days (threshold: {min_age_days})"
        ),
        (false, true, false) => {
            format!("Shown {times_shown} times but never applied (threshold: {min_shows})")
        }
        (false, false, true) => {
            let rate = (times_applied as f64 / times_shown as f64) * 100.0;
            let max_pct = max_rate * 100.0;
            format!(
                "Application rate {rate:.1}% below threshold {max_pct:.1}% after {times_shown} shows"
            )
        }
        _ => {
            // Multiple criteria matched — list them all
            let mut parts = Vec::new();
            if c1 {
                parts.push(format!("low-confidence and {age_days:.0} days old"));
            }
            if c2 {
                parts.push(format!("shown {times_shown}x, never applied"));
            }
            if c3 {
                let rate = (times_applied as f64 / times_shown as f64) * 100.0;
                parts.push(format!("application rate {rate:.1}%"));
            }
            parts.join("; ")
        }
    }
}

/// Sets `retired_at = datetime('now')` for all given IDs in a single transaction.
/// Returns the number of rows updated.
fn retire_candidates(conn: &Connection, ids: &[i64]) -> TaskMgrResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }

    // Build a parameterized IN clause
    let placeholders = ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql =
        format!("UPDATE learnings SET retired_at = datetime('now') WHERE id IN ({placeholders})");

    let params = rusqlite::params_from_iter(ids.iter());
    let rows_updated = conn.execute(&sql, params)?;
    Ok(rows_updated)
}

/// Restores soft-archived learnings by setting retired_at = NULL.
///
/// Validates each ID: must exist and must currently be retired.
/// Processes all IDs in a single transaction; collects per-ID errors without aborting.
pub fn curate_unretire(conn: &Connection, learning_ids: Vec<i64>) -> TaskMgrResult<UnretireResult> {
    let mut restored = Vec::new();
    let mut errors = Vec::new();

    // Validate each ID before opening a transaction
    for &id in &learning_ids {
        let result: rusqlite::Result<Option<bool>> = conn.query_row(
            "SELECT retired_at IS NOT NULL FROM learnings WHERE id = ?1",
            [id],
            |row| row.get::<_, bool>(0).map(Some),
        );

        match result {
            Err(_) | Ok(None) => {
                errors.push(format!("Learning {id} not found"));
            }
            Ok(Some(false)) => {
                errors.push(format!("Learning {id} is not retired (retired_at IS NULL)"));
            }
            Ok(Some(true)) => {
                restored.push(id);
            }
        }
    }

    if !restored.is_empty() {
        let placeholders = restored
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("UPDATE learnings SET retired_at = NULL WHERE id IN ({placeholders})");
        let params = rusqlite::params_from_iter(restored.iter());
        conn.execute(&sql, params)?;
    }

    Ok(UnretireResult { restored, errors })
}

/// Queries active learnings that are missing at least one metadata field.
///
/// - Excludes retired learnings (`retired_at IS NOT NULL`).
/// - Excludes learnings with all three metadata fields populated.
/// - When `params.field_filter` is `Some(field)`, restricts to learnings
///   missing only that specific field.
/// - Returns `Ok(vec![])` when no candidates exist (never errors on empty result).
pub fn find_enrichment_candidates(
    conn: &Connection,
    params: &EnrichParams,
) -> crate::TaskMgrResult<Vec<EnrichCandidate>> {
    use types::EnrichFieldFilter;

    let sql = match &params.field_filter {
        None => {
            "
            SELECT id, title,
                   applies_to_files IS NULL AS missing_files,
                   applies_to_task_types IS NULL AS missing_task_types,
                   applies_to_errors IS NULL AS missing_errors
            FROM learnings
            WHERE retired_at IS NULL
              AND (
                applies_to_files IS NULL
                OR applies_to_task_types IS NULL
                OR applies_to_errors IS NULL
              )
            ORDER BY id ASC
        "
        }
        Some(EnrichFieldFilter::AppliesToFiles) => {
            "
            SELECT id, title,
                   applies_to_files IS NULL AS missing_files,
                   applies_to_task_types IS NULL AS missing_task_types,
                   applies_to_errors IS NULL AS missing_errors
            FROM learnings
            WHERE retired_at IS NULL
              AND applies_to_files IS NULL
            ORDER BY id ASC
        "
        }
        Some(EnrichFieldFilter::AppliesToTaskTypes) => {
            "
            SELECT id, title,
                   applies_to_files IS NULL AS missing_files,
                   applies_to_task_types IS NULL AS missing_task_types,
                   applies_to_errors IS NULL AS missing_errors
            FROM learnings
            WHERE retired_at IS NULL
              AND applies_to_task_types IS NULL
            ORDER BY id ASC
        "
        }
        Some(EnrichFieldFilter::AppliesToErrors) => {
            "
            SELECT id, title,
                   applies_to_files IS NULL AS missing_files,
                   applies_to_task_types IS NULL AS missing_task_types,
                   applies_to_errors IS NULL AS missing_errors
            FROM learnings
            WHERE retired_at IS NULL
              AND applies_to_errors IS NULL
            ORDER BY id ASC
        "
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let candidates = stmt
        .query_map([], |row| {
            Ok(EnrichCandidate {
                id: row.get("id")?,
                title: row.get("title")?,
                missing_files: row.get("missing_files")?,
                missing_task_types: row.get("missing_task_types")?,
                missing_errors: row.get("missing_errors")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(candidates)
}

/// Merges a cluster of duplicate learnings into a single canonical learning.
///
/// Given pre-validated `params` (source IDs + LLM-generated merged content),
/// this function:
/// 1. Loads each source learning; skips any that are already retired.
/// 2. Creates a new merged learning whose metadata fields are the union of all
///    source fields and whose bandit stats are the sums of the source stats.
/// 3. Soft-archives each active source by setting `retired_at = datetime('now')`.
/// 4. Returns the merged learning ID plus the lists of retired / skipped IDs.
///
/// All DB writes are performed inside a single transaction so the operation is
/// atomic.
///
/// # Stub
/// Not yet implemented — tracked as FEAT-004.
pub fn merge_cluster(
    _conn: &Connection,
    _params: MergeClusterParams,
) -> TaskMgrResult<MergeClusterResult> {
    todo!("FEAT-004: implement merge_cluster")
}

#[cfg(test)]
mod tests;
