//! Curate subcommand implementations.
//!
//! Provides `curate retire` and `curate unretire` commands for managing
//! the institutional memory quality via soft-archiving stale learnings.

pub mod dedup;
pub mod enrich;
mod json_utils;
pub mod output;
pub mod types;

pub use dedup::{build_dedup_prompt, cluster_by_embedding_similarity, parse_dedup_response};
pub use output::{
    format_count_text, format_dedup_text, format_embed_text, format_enrich_text,
    format_retire_text, format_unretire_text,
};
pub use types::{
    CountResult, DedupCluster, DedupParams, DedupResult, DeduplicateLearningItem, EmbedParams,
    EmbedResult, EnrichCandidate, EnrichParams, EnrichResult, MergeClusterParams,
    MergeClusterResult, RawDedupCluster, RetireParams, RetireResult, RetirementCandidate,
    UnretireResult,
};

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, mpsc};

use rusqlite::{Connection, OptionalExtension};

use crate::learnings::LearningWriter;
use crate::learnings::crud::{RecordLearningParams, get_learning_tags, record_learning};
use crate::loop_engine::claude::spawn_claude;
use crate::loop_engine::config::PermissionMode;

/// Rank confidence levels for comparison: High=2, Medium=1, Low=0.
fn confidence_rank(s: &str) -> u8 {
    match s {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}
use crate::TaskMgrResult;
use crate::models::{Confidence, LearningOutcome};

/// Returns learning statistics: total, active, retired, and embedded counts.
pub fn curate_count(conn: &Connection) -> TaskMgrResult<CountResult> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))?;
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    let retired: i64 = conn.query_row(
        "SELECT COUNT(*) FROM learnings WHERE retired_at IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let embedded: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT le.learning_id) FROM learning_embeddings le \
         JOIN learnings l ON l.id = le.learning_id WHERE l.retired_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(CountResult {
        total,
        active,
        retired,
        embedded,
    })
}

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

                let reason = build_reason(&ReasonContext {
                    confidence: &confidence,
                    age_days,
                    times_shown,
                    times_applied,
                    min_age_days: i64::from(params.min_age_days),
                    min_shows: i64::from(params.min_shows),
                    min_shows_doubled,
                    max_rate: params.max_rate,
                });

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

/// Context for building a human-readable retirement reason string.
struct ReasonContext<'a> {
    confidence: &'a str,
    age_days: f64,
    times_shown: i64,
    times_applied: i64,
    min_age_days: i64,
    min_shows: i64,
    min_shows_doubled: i64,
    max_rate: f64,
}

/// Determines which criterion matched and returns a human-readable reason string.
fn build_reason(ctx: &ReasonContext<'_>) -> String {
    let ReasonContext {
        confidence,
        age_days,
        times_shown,
        times_applied,
        min_age_days,
        min_shows,
        min_shows_doubled,
        max_rate,
    } = *ctx;
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
    let tx = conn.unchecked_transaction()?;
    let mut restored = Vec::new();
    let mut errors = Vec::new();

    for &id in &learning_ids {
        let result: rusqlite::Result<Option<bool>> = tx.query_row(
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
        tx.execute(&sql, params)?;
    }

    tx.commit()?;
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
/// atomic. The caller (typically `curate_dedup`) is responsible for scheduling
/// embeddings via [`LearningWriter::push_existing`] after this returns.
pub fn merge_cluster(
    conn: &Connection,
    params: MergeClusterParams,
) -> TaskMgrResult<MergeClusterResult> {
    // Phase 1: determine which source learnings are active vs already-retired.
    // Uses raw SQL because the Learning struct does not expose retired_at.
    struct SourceRow {
        id: i64,
        applies_to_files: Option<String>,
        applies_to_task_types: Option<String>,
        applies_to_errors: Option<String>,
        confidence: String,
        times_shown: i32,
        times_applied: i32,
    }

    let mut active_rows: Vec<SourceRow> = Vec::new();
    let mut skipped_source_ids: Vec<i64> = Vec::new();

    for &id in &params.source_ids {
        let row: Option<SourceRow> = conn
            .query_row(
                "SELECT id, applies_to_files, applies_to_task_types, applies_to_errors,
                         confidence, times_shown, times_applied
                 FROM learnings WHERE id = ?1 AND retired_at IS NULL",
                [id],
                |row| {
                    Ok(SourceRow {
                        id: row.get("id")?,
                        applies_to_files: row.get("applies_to_files")?,
                        applies_to_task_types: row.get("applies_to_task_types")?,
                        applies_to_errors: row.get("applies_to_errors")?,
                        confidence: row.get("confidence")?,
                        times_shown: row.get("times_shown")?,
                        times_applied: row.get("times_applied")?,
                    })
                },
            )
            .optional()?;

        match row {
            Some(r) => active_rows.push(r),
            None => skipped_source_ids.push(id),
        }
    }

    // Aggregate metadata from active sources.
    let mut files_set: HashSet<String> = HashSet::new();
    let mut task_types_set: HashSet<String> = HashSet::new();
    let mut errors_set: HashSet<String> = HashSet::new();
    let mut tags_set: HashSet<String> = HashSet::new();
    let mut total_shown: i64 = 0;
    let mut total_applied: i64 = 0;

    let mut best_confidence_str = "low";

    for row in &active_rows {
        if let Some(ref json) = row.applies_to_files {
            let v: Vec<String> = serde_json::from_str(json).unwrap_or_default();
            files_set.extend(v);
        }
        if let Some(ref json) = row.applies_to_task_types {
            let v: Vec<String> = serde_json::from_str(json).unwrap_or_default();
            task_types_set.extend(v);
        }
        if let Some(ref json) = row.applies_to_errors {
            let v: Vec<String> = serde_json::from_str(json).unwrap_or_default();
            errors_set.extend(v);
        }
        let tags = get_learning_tags(conn, row.id)?;
        tags_set.extend(tags);
        total_shown += i64::from(row.times_shown);
        total_applied += i64::from(row.times_applied);
        if confidence_rank(&row.confidence) > confidence_rank(best_confidence_str) {
            best_confidence_str = match row.confidence.as_str() {
                "high" => "high",
                "medium" => "medium",
                _ => "low",
            };
        }
    }

    let best_confidence: Confidence = best_confidence_str.parse().unwrap_or(Confidence::Low);

    // Convert sets to sorted vecs for deterministic output.
    let union_files: Vec<String> = {
        let mut v: Vec<_> = files_set.into_iter().collect();
        v.sort();
        v
    };
    let union_task_types: Vec<String> = {
        let mut v: Vec<_> = task_types_set.into_iter().collect();
        v.sort();
        v
    };
    let union_errors: Vec<String> = {
        let mut v: Vec<_> = errors_set.into_iter().collect();
        v.sort();
        v
    };
    let union_tags: Vec<String> = {
        let mut v: Vec<_> = tags_set.into_iter().collect();
        v.sort();
        v
    };

    let active_ids: Vec<i64> = active_rows.iter().map(|r| r.id).collect();

    // Phase 2: all DB writes in a single transaction.
    let tx = conn.unchecked_transaction()?;

    // Clone before moving into RecordLearningParams so we can populate MergeClusterResult.
    let merged_title = params.merged_title.clone();
    let merged_content = params.merged_content.clone();

    let record_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: params.merged_title,
        content: params.merged_content,
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: if union_files.is_empty() {
            None
        } else {
            Some(union_files)
        },
        applies_to_task_types: if union_task_types.is_empty() {
            None
        } else {
            Some(union_task_types)
        },
        applies_to_errors: if union_errors.is_empty() {
            None
        } else {
            Some(union_errors)
        },
        tags: if union_tags.is_empty() {
            None
        } else {
            Some(union_tags)
        },
        confidence: best_confidence,
    };
    let merged = record_learning(&tx, record_params)?;
    let merged_id = merged.learning_id;

    // Update bandit stats (record_learning always inserts 0,0).
    tx.execute(
        "UPDATE learnings SET times_shown = ?1, times_applied = ?2 WHERE id = ?3",
        rusqlite::params![total_shown, total_applied, merged_id],
    )?;

    // Soft-archive all active sources.
    for &id in &active_ids {
        tx.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
            [id],
        )?;
    }

    tx.commit()?;

    Ok(MergeClusterResult {
        merged_learning_id: merged_id,
        merged_title,
        merged_content,
        retired_source_ids: active_ids,
        skipped_source_ids,
    })
}

/// Result of one LLM batch dispatched by `process_batches_parallel`.
struct BatchOutput {
    /// Original batch index, used to restore processing order.
    batch_idx: usize,
    /// Parsed raw clusters on success, or `Err(())` on any LLM/parse failure.
    raw_clusters: Result<Vec<RawDedupCluster>, ()>,
}

/// Dispatches LLM batch calls in parallel using `std::thread` + `mpsc`.
///
/// Spawns `min(concurrency, batches.len())` worker threads. Each worker pulls
/// batches from a shared work queue, calls `spawn_claude`, and sends the result
/// back via a channel. Workers do not access the database. The returned vec is
/// sorted by `batch_idx` so the caller can apply merge tracking in order.
///
/// An error in one batch does not block other batches; the failed batch is
/// represented as `BatchOutput { raw_clusters: Err(()) }`.
fn process_batches_parallel(
    batches: Vec<Vec<DeduplicateLearningItem>>,
    threshold: f64,
    concurrency: usize,
    model: &str,
) -> Vec<BatchOutput> {
    let batch_count = batches.len();
    if batch_count == 0 {
        return Vec::new();
    }

    type WorkQueue = Arc<Mutex<VecDeque<(usize, Vec<DeduplicateLearningItem>)>>>;
    let work_queue: WorkQueue = Arc::new(Mutex::new(batches.into_iter().enumerate().collect()));

    let (tx, rx) = mpsc::channel::<BatchOutput>();
    let num_threads = concurrency.max(1).min(batch_count);

    let model = Arc::new(model.to_owned());
    let mut handles = Vec::with_capacity(num_threads);
    for _ in 0..num_threads {
        let queue = Arc::clone(&work_queue);
        let tx = tx.clone();
        let model = Arc::clone(&model);
        let handle = std::thread::spawn(move || {
            loop {
                let item = {
                    let mut guard = queue.lock().expect("work queue lock poisoned");
                    guard.pop_front()
                };
                let (batch_idx, batch_items) = match item {
                    Some(x) => x,
                    None => break,
                };

                let eligible_ids: Vec<i64> = batch_items.iter().map(|i| i.id).collect();
                let prompt = build_dedup_prompt(&batch_items, threshold);

                let raw_clusters = match spawn_claude(
                    &prompt,
                    None,
                    None,
                    Some(&model),
                    None,
                    false,
                    &PermissionMode::text_only(),
                    None,
                    None,
                    None,
                    false,
                ) {
                    Err(e) => {
                        eprintln!(
                            "Warning: spawn_claude failed for batch {}: {}",
                            batch_idx + 1,
                            e
                        );
                        Err(())
                    }
                    Ok(r) if r.exit_code != 0 => {
                        eprintln!(
                            "Warning: claude exited with code {} for batch {}",
                            r.exit_code,
                            batch_idx + 1
                        );
                        Err(())
                    }
                    Ok(r) => match parse_dedup_response(&r.output, &eligible_ids) {
                        Ok(clusters) => Ok(clusters),
                        Err(e) => {
                            eprintln!(
                                "Warning: failed to parse dedup response for batch {}: {}",
                                batch_idx + 1,
                                e
                            );
                            Err(())
                        }
                    },
                };

                let _ = tx.send(BatchOutput {
                    batch_idx,
                    raw_clusters,
                });
            }
        });
        handles.push(handle);
    }

    // Drop the sender clone owned by main so rx closes when all workers finish.
    drop(tx);

    let mut results: Vec<BatchOutput> = rx.into_iter().collect();

    for handle in handles {
        let _ = handle.join();
    }

    // Restore original batch order so the caller applies merged_ids tracking
    // deterministically regardless of which worker finished first.
    results.sort_by_key(|r| r.batch_idx);
    results
}

/// Orchestrates the full dedup flow: loads active learnings, batches them,
/// calls Claude to identify duplicate clusters, and merges each cluster via
/// `merge_cluster()`.
///
/// - When `params.dry_run=true`, clusters are identified but no DB writes occur.
/// - When there are 0 active learnings, returns an empty `DedupResult` without
///   invoking the LLM.
pub fn curate_dedup(conn: &Connection, params: DedupParams) -> TaskMgrResult<DedupResult> {
    // Load all active learnings (id, title, content, confidence).
    struct LearningRow {
        id: i64,
        title: String,
        content: String,
        confidence: String,
    }

    let mut stmt = conn.prepare(
        "SELECT id, title, content, confidence
         FROM learnings
         WHERE retired_at IS NULL
         ORDER BY id ASC",
    )?;
    let rows: Vec<LearningRow> = stmt
        .query_map([], |row| {
            Ok(LearningRow {
                id: row.get("id")?,
                title: row.get("title")?,
                content: row.get("content")?,
                confidence: row.get("confidence")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Short-circuit: no active learnings means nothing to dedup.
    if rows.is_empty() {
        return Ok(DedupResult {
            dry_run: params.dry_run,
            clusters_found: 0,
            learnings_merged: 0,
            learnings_created: 0,
            llm_errors: 0,
            clusters: Vec::new(),
        });
    }

    // Build a map from id -> (title, confidence) for cluster assembly.
    let id_info: HashMap<i64, (String, String)> = rows
        .iter()
        .map(|r| (r.id, (r.title.clone(), r.confidence.clone())))
        .collect();

    // Build DeduplicateLearningItem list for prompt building.
    let items: Vec<DeduplicateLearningItem> = rows
        .iter()
        .map(|r| DeduplicateLearningItem {
            id: r.id,
            title: r.title.clone(),
            content: r.content.clone(),
        })
        .collect();

    // Build LLM batches.
    //
    // Pre-filter path (embed_model set and embeddings exist in the DB):
    //   - Load stored embeddings; split items into embedded vs unembedded.
    //   - Cluster the embedded subset by cosine similarity (threshold slightly
    //     below the LLM threshold to avoid false negatives at the pre-filter stage).
    //   - Each cluster (2+ members) → one LLM batch; singletons are skipped.
    //   - Unembedded learnings → standard-size fallback batches sent to the LLM.
    //
    // Standard path (zero embeddings stored, or embed_model empty):
    //   - Original auto-calculated batch size logic, unchanged.
    let batches: Vec<Vec<DeduplicateLearningItem>> = if !params.embed_model.is_empty() {
        use crate::learnings::embeddings::load_all_active_embeddings;

        let emb_list = match load_all_active_embeddings(conn, &params.embed_model) {
            Ok(list) => list,
            Err(e) => {
                eprintln!("Warning: failed to load embeddings for pre-filter: {e}");
                Vec::new()
            }
        };

        if emb_list.is_empty() {
            // No embeddings stored — standard batch path.
            let total_chars: usize = items.iter().map(|i| i.content.len()).sum();
            let batch_size = params
                .batch_size
                .unwrap_or_else(|| {
                    if total_chars < 150_000 {
                        items.len()
                    } else {
                        let avg = total_chars / items.len();
                        (200_000 / avg.max(1)).clamp(20, 100)
                    }
                })
                .max(1);
            items.chunks(batch_size).map(|s| s.to_vec()).collect()
        } else {
            // Pre-filter: cluster embedded learnings; send clusters to the LLM;
            // route unembedded learnings to standard-size fallback batches.
            let emb_map: HashMap<i64, Vec<f32>> = emb_list
                .into_iter()
                .map(|le| (le.learning_id, le.embedding))
                .collect();

            // Clone items into a lookup map for fast cluster→item resolution.
            let item_map: HashMap<i64, DeduplicateLearningItem> =
                items.iter().map(|i| (i.id, i.clone())).collect();

            let mut emb_pairs: Vec<(i64, Vec<f32>)> = Vec::new();
            let mut without_emb: Vec<DeduplicateLearningItem> = Vec::new();

            for item in &items {
                match emb_map.get(&item.id) {
                    Some(emb) => emb_pairs.push((item.id, emb.clone())),
                    None => without_emb.push(item.clone()),
                }
            }

            // Use a threshold slightly below the LLM threshold so borderline pairs
            // still reach the LLM rather than being silently dropped.
            let emb_threshold = ((params.threshold as f32) - 0.05_f32).max(0.0_f32);
            let clusters = cluster_by_embedding_similarity(&emb_pairs, emb_threshold);

            let clustered_count: usize = clusters.iter().map(|c| c.len()).sum();
            let singleton_count = emb_pairs.len().saturating_sub(clustered_count);
            eprintln!(
                "Embedding pre-filter: {} cluster(s) ({} items), {} singleton(s) skipped, {} unembedded",
                clusters.len(),
                clustered_count,
                singleton_count,
                without_emb.len(),
            );

            let mut batches: Vec<Vec<DeduplicateLearningItem>> = Vec::new();

            // Each embedding cluster → one LLM batch.
            for cluster_ids in clusters {
                let batch: Vec<DeduplicateLearningItem> = cluster_ids
                    .iter()
                    .filter_map(|id| item_map.get(id).cloned())
                    .collect();
                if batch.len() >= 2 {
                    batches.push(batch);
                }
            }

            // Unembedded learnings → standard-size fallback batches.
            if !without_emb.is_empty() {
                let total_chars: usize = without_emb.iter().map(|i| i.content.len()).sum();
                let batch_size = params
                    .batch_size
                    .unwrap_or_else(|| {
                        if total_chars < 150_000 {
                            without_emb.len()
                        } else {
                            let avg = total_chars / without_emb.len();
                            (200_000 / avg.max(1)).clamp(20, 100)
                        }
                    })
                    .max(1);
                for chunk in without_emb.chunks(batch_size) {
                    batches.push(chunk.to_vec());
                }
            }

            batches
        }
    } else {
        // embed_model empty — standard batch path (original behaviour).
        let total_chars: usize = items.iter().map(|i| i.content.len()).sum();
        let batch_size = params
            .batch_size
            .unwrap_or_else(|| {
                if total_chars < 150_000 {
                    items.len()
                } else {
                    let avg = total_chars / items.len();
                    (200_000 / avg.max(1)).clamp(20, 100)
                }
            })
            .max(1);
        items.chunks(batch_size).map(|s| s.to_vec()).collect()
    };

    let total_batches = batches.len();
    if total_batches > 1 {
        eprintln!(
            "Processing {} batches (concurrency={})...",
            total_batches, params.concurrency
        );
    }

    let batch_outputs =
        process_batches_parallel(batches, params.threshold, params.concurrency, &params.model);

    // Track IDs merged across batches to handle cross-batch duplicates.
    let mut merged_ids: HashSet<i64> = HashSet::new();

    let mut writer = LearningWriter::new(params.db_dir.as_deref());

    let mut all_clusters: Vec<DedupCluster> = Vec::new();
    let mut llm_errors: usize = 0;
    let mut learnings_merged: usize = 0;
    let mut learnings_created: usize = 0;

    for output in batch_outputs {
        let raw_clusters = match output.raw_clusters {
            Ok(clusters) => clusters,
            Err(()) => {
                llm_errors += 1;
                continue;
            }
        };

        for raw in raw_clusters {
            let source_ids = match raw.source_ids {
                Some(ids) if ids.len() >= 2 => ids,
                _ => continue,
            };

            // Skip if any source ID was already merged by a prior batch.
            if source_ids.iter().any(|id| merged_ids.contains(id)) {
                continue;
            }

            let merged_title = raw
                .merged_title
                .unwrap_or_else(|| "Merged learning".to_string());
            let merged_content = raw.merged_content.unwrap_or_default();
            let merged_outcome = raw.merged_outcome.unwrap_or_else(|| "pattern".to_string());
            let reason = raw.reason.unwrap_or_default();

            let source_titles: Vec<String> = source_ids
                .iter()
                .map(|id| id_info.get(id).map(|(t, _)| t.clone()).unwrap_or_default())
                .collect();

            // Best confidence among sources: high > medium > low.
            let merged_confidence = source_ids
                .iter()
                .filter_map(|id| id_info.get(id).map(|(_, c)| c.as_str()))
                .max_by_key(|c| confidence_rank(c))
                .unwrap_or("low")
                .to_string();

            let merged_learning_id = if params.dry_run {
                None
            } else {
                let merge_params = MergeClusterParams {
                    source_ids: source_ids.clone(),
                    merged_title: merged_title.clone(),
                    merged_content: merged_content.clone(),
                };
                match merge_cluster(conn, merge_params) {
                    Ok(result) => {
                        learnings_merged += result.retired_source_ids.len();
                        learnings_created += 1;
                        let id = result.merged_learning_id;
                        writer.push_existing(id, result.merged_title, result.merged_content);
                        Some(id)
                    }
                    Err(e) => {
                        eprintln!("Warning: merge_cluster failed: {}", e);
                        continue;
                    }
                }
            };

            // Track merged IDs so subsequent batches skip them.
            for &id in &source_ids {
                merged_ids.insert(id);
            }

            all_clusters.push(DedupCluster {
                source_ids,
                source_titles,
                merged_title,
                merged_content,
                merged_outcome,
                merged_confidence,
                reason,
                merged_learning_id,
            });
        }
    }

    let clusters_found = all_clusters.len();
    // Flush deferred embeddings once, after all merges are committed.
    let _ = writer.flush(conn);
    Ok(DedupResult {
        dry_run: params.dry_run,
        clusters_found,
        learnings_merged,
        learnings_created,
        llm_errors,
        clusters: all_clusters,
    })
}

/// Embeds active learnings via Ollama and stores the vectors in `learning_embeddings`.
///
/// Behaviour:
/// - `params.status = true`: returns counts without embedding.
/// - `params.force = true`: re-embeds ALL active learnings (replaces existing).
/// - Default: embeds only active learnings that have no entry for `params.model`.
///
/// Learnings whose embedding text (title + content) is empty are skipped with
/// a warning printed to stderr.  All other errors (Ollama call failures, store
/// failures) are counted and reported in the result without aborting the run.
pub fn curate_embed(conn: &Connection, params: EmbedParams) -> TaskMgrResult<EmbedResult> {
    use crate::learnings::embeddings::{OllamaEmbedder, count_embedded, store_embedding};

    // Status counts are always computed (needed for both modes).
    let total_active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL",
        [],
        |row| row.get(0),
    )?;

    let already_embedded = count_embedded(conn, &params.model)?;

    if params.status {
        return Ok(EmbedResult {
            status_only: true,
            total_active,
            already_embedded,
            embedded_this_run: 0,
            skipped_empty: 0,
            errors: 0,
            model: params.model,
        });
    }

    // Verify Ollama is reachable and the model is available.
    let embedder = OllamaEmbedder::new(&params.ollama_url, &params.model);
    match embedder.is_available() {
        Err(e) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!(
                    "Ollama not reachable at {}: {e}. Is Ollama running?",
                    params.ollama_url
                ),
            )
            .into());
        }
        Ok(false) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Model '{}' not found in Ollama. Run: ollama pull {}",
                    params.model, params.model
                ),
            )
            .into());
        }
        Ok(true) => {}
    }

    // Load learnings: either all active (force) or only those without an embedding.
    struct LearningRow {
        id: i64,
        title: String,
        content: String,
    }

    // Use explicit let bindings inside each branch so the borrow of `stmt`
    // is provably released before `stmt` is dropped (avoids E0597).
    let rows: Vec<LearningRow> = if params.force {
        let mut stmt = conn.prepare(
            "SELECT id, title, COALESCE(content, '') AS content
             FROM learnings
             WHERE retired_at IS NULL
             ORDER BY id ASC",
        )?;
        let collected: Vec<LearningRow> = stmt
            .query_map([], |row| {
                Ok(LearningRow {
                    id: row.get("id")?,
                    title: row.get("title")?,
                    content: row.get("content")?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        collected
    } else {
        let mut stmt = conn.prepare(
            "SELECT l.id, l.title, COALESCE(l.content, '') AS content
             FROM learnings l
             LEFT JOIN learning_embeddings le
               ON le.learning_id = l.id AND le.model = ?1
             WHERE l.retired_at IS NULL
               AND le.learning_id IS NULL
             ORDER BY l.id ASC",
        )?;
        let collected: Vec<LearningRow> = stmt
            .query_map([&params.model], |row| {
                Ok(LearningRow {
                    id: row.get("id")?,
                    title: row.get("title")?,
                    content: row.get("content")?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        collected
    };

    // Build embedding items. Title-only learnings use just the title; skip if empty.
    struct EmbedItem {
        id: i64,
        text: String,
    }

    let mut items: Vec<EmbedItem> = Vec::new();
    let mut skipped_empty: usize = 0;

    for row in &rows {
        let text = if row.content.is_empty() {
            row.title.trim().to_string()
        } else {
            format!("{}\n\n{}", row.title, row.content)
        };

        if text.is_empty() {
            eprintln!(
                "Warning: skipping learning {} '{}': zero-length content",
                row.id, row.title
            );
            skipped_empty += 1;
            continue;
        }

        items.push(EmbedItem { id: row.id, text });
    }

    let total_to_embed = items.len();
    if total_to_embed == 0 {
        eprintln!("No learnings to embed.");
    } else {
        eprintln!("Embedding {} learning(s)...", total_to_embed);
    }

    // Batch-embed and store; count errors without aborting.
    const BATCH_SIZE: usize = 50;
    let mut embedded_this_run: usize = 0;
    let mut errors: usize = 0;
    let mut done: usize = 0;

    for chunk in items.chunks(BATCH_SIZE) {
        done += chunk.len();
        eprintln!("  [{}/{}] embedding batch...", done, total_to_embed);

        let texts: Vec<&str> = chunk.iter().map(|i| i.text.as_str()).collect();

        match embedder.embed_batch(&texts) {
            Ok(embeddings) => {
                if embeddings.len() != chunk.len() {
                    eprintln!(
                        "Warning: Ollama returned {} embeddings for {} inputs; processing available",
                        embeddings.len(),
                        chunk.len()
                    );
                }
                for (item, embedding) in chunk.iter().zip(embeddings.iter()) {
                    match store_embedding(conn, item.id, &params.model, embedding) {
                        Ok(()) => embedded_this_run += 1,
                        Err(e) => {
                            eprintln!(
                                "Warning: failed to store embedding for learning {}: {e}",
                                item.id
                            );
                            errors += 1;
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: embedding batch failed: {e}");
                errors += chunk.len();
            }
        }
    }

    Ok(EmbedResult {
        status_only: false,
        total_active,
        already_embedded,
        embedded_this_run,
        skipped_empty,
        errors,
        model: params.model,
    })
}

#[cfg(test)]
mod tests;
