//! Benchmark tests for smart task selection query performance.
//!
//! These tests verify that the task selection algorithm performs well with large task lists.
//! Target: < 50ms for task selection with 200 tasks.

use std::time::Instant;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection};

/// Helper to insert a task with specified properties.
fn insert_task(conn: &rusqlite::Connection, id: &str, title: &str, status: &str, priority: i32) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, title, status, priority],
    )
    .expect("Failed to insert task");
}

/// Helper to insert a task file relationship.
fn insert_task_file(conn: &rusqlite::Connection, task_id: &str, file_path: &str) {
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
        rusqlite::params![task_id, file_path],
    )
    .expect("Failed to insert task file");
}

/// Helper to insert a task relationship.
fn insert_relationship(
    conn: &rusqlite::Connection,
    task_id: &str,
    related_id: &str,
    rel_type: &str,
) {
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?1, ?2, ?3)",
        rusqlite::params![task_id, related_id, rel_type],
    )
    .expect("Failed to insert relationship");
}

/// Create a 200-task benchmark database with realistic relationships.
fn create_benchmark_db() -> (TempDir, rusqlite::Connection) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let conn = open_connection(temp_dir.path()).expect("Failed to open connection");
    create_schema(&conn).expect("Failed to create schema");

    // Create 200 tasks with varying statuses and priorities
    // Distribution: 120 todo, 50 done, 15 blocked, 10 skipped, 5 irrelevant
    let mut task_count = 0;
    let statuses = [
        ("todo", 120),
        ("done", 50),
        ("blocked", 15),
        ("skipped", 10),
        ("irrelevant", 5),
    ];

    for (status, count) in statuses {
        for _ in 0..count {
            let id = format!("US-{:03}", task_count);
            let title = format!("Task {} - {} status", task_count, status);
            let priority = (task_count % 100) as i32 + 1; // Priorities 1-100
            insert_task(&conn, &id, &title, status, priority);
            task_count += 1;
        }
    }

    // Add file relationships (avg 2-3 files per task)
    // Total: ~500 file relationships
    let files = [
        "src/main.rs",
        "src/lib.rs",
        "src/cli.rs",
        "src/error.rs",
        "src/db/mod.rs",
        "src/db/connection.rs",
        "src/db/schema.rs",
        "src/commands/mod.rs",
        "src/commands/init.rs",
        "src/commands/list.rs",
        "src/commands/next.rs",
        "src/commands/complete.rs",
        "src/commands/fail.rs",
        "src/models/mod.rs",
        "src/models/task.rs",
        "src/learnings/mod.rs",
        "src/learnings/crud.rs",
        "src/learnings/recall.rs",
        "tests/cli_tests.rs",
        "tests/e2e_loop.rs",
    ];

    for task_num in 0..200 {
        let task_id = format!("US-{:03}", task_num);
        // Each task touches 1-4 files (deterministic based on task number)
        let file_count = 1 + (task_num % 4);
        for f in 0..file_count {
            let file_idx = (task_num + f) % files.len();
            insert_task_file(&conn, &task_id, files[file_idx]);
        }
    }

    // Add relationships (realistic distribution)
    // ~150 dependsOn, ~100 synergyWith, ~50 batchWith, ~30 conflictsWith
    for task_num in 10..160 {
        // dependsOn: task depends on one of the first 10 completed tasks
        let task_id = format!("US-{:03}", task_num);
        let dep_id = format!("US-{:03}", task_num % 10);
        insert_relationship(&conn, &task_id, &dep_id, "dependsOn");
    }

    for task_num in 0..100 {
        // synergyWith: adjacent tasks have synergy
        let task_id = format!("US-{:03}", task_num);
        let synergy_id = format!("US-{:03}", (task_num + 1) % 200);
        insert_relationship(&conn, &task_id, &synergy_id, "synergyWith");
    }

    for task_num in 0..50 {
        // batchWith: pairs of tasks
        let task_id = format!("US-{:03}", task_num * 2);
        let batch_id = format!("US-{:03}", task_num * 2 + 1);
        insert_relationship(&conn, &task_id, &batch_id, "batchWith");
    }

    for task_num in 0..30 {
        // conflictsWith: some tasks conflict
        let task_id = format!("US-{:03}", task_num);
        let conflict_id = format!("US-{:03}", 199 - task_num);
        insert_relationship(&conn, &task_id, &conflict_id, "conflictsWith");
    }

    (temp_dir, conn)
}

#[test]
fn test_benchmark_task_selection_200_tasks() {
    let (temp_dir, conn) = create_benchmark_db();
    drop(conn); // Close connection to test full open + query cycle

    // Measure task selection time (includes connection open per iteration)
    let iterations = 10;
    let mut total_time = std::time::Duration::ZERO;
    let mut selection_times = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let start = Instant::now();
        let conn = open_connection(temp_dir.path()).expect("Failed to open connection");
        let result = task_mgr::commands::next::select_next_task(
            &conn,
            &["src/main.rs".to_string(), "src/lib.rs".to_string()],
            &["US-005".to_string()],
        );
        let elapsed = start.elapsed();
        selection_times.push(elapsed);
        total_time += elapsed;

        // Verify selection succeeded
        assert!(result.is_ok(), "Selection should succeed");
        let selection = result.unwrap();
        assert!(selection.eligible_count > 0, "Should have eligible tasks");
    }

    let avg_time = total_time / iterations as u32;
    let min_time = selection_times.iter().min().unwrap();
    let max_time = selection_times.iter().max().unwrap();

    // Print benchmark results
    println!("\n=== Task Selection Benchmark (200 tasks) ===");
    println!("Iterations: {}", iterations);
    println!("Average time: {:?}", avg_time);
    println!("Min time: {:?}", min_time);
    println!("Max time: {:?}", max_time);

    // Target: < 50ms average
    assert!(
        avg_time.as_millis() < 50,
        "Average selection time {:?} exceeds 50ms target",
        avg_time
    );
}

#[test]
fn test_benchmark_individual_queries() {
    let (_temp_dir, conn) = create_benchmark_db();

    // Benchmark each individual query used in task selection

    // 1. Get completed task IDs
    let start = Instant::now();
    let completed_ids: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM tasks WHERE status IN ('done', 'irrelevant')")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    let get_completed_time = start.elapsed();

    // 2. Get todo tasks
    let start = Instant::now();
    let todo_count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'todo'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let get_todo_time = start.elapsed();

    // 3. Get all relationships by type (4 queries)
    let start = Instant::now();
    for rel_type in ["dependsOn", "synergyWith", "batchWith", "conflictsWith"] {
        let mut stmt = conn
            .prepare("SELECT task_id, related_id FROM task_relationships WHERE rel_type = ?")
            .unwrap();
        let _: Vec<(String, String)> = stmt
            .query_map([rel_type], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
    }
    let get_relationships_time = start.elapsed();

    // 4. Get all task files
    let start = Instant::now();
    let mut stmt = conn
        .prepare("SELECT task_id, file_path FROM task_files")
        .unwrap();
    let files: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    let get_files_time = start.elapsed();

    println!("\n=== Individual Query Benchmarks ===");
    println!(
        "Get completed IDs ({}): {:?}",
        completed_ids.len(),
        get_completed_time
    );
    println!("Get todo count ({}): {:?}", todo_count, get_todo_time);
    println!(
        "Get all relationships (4 queries): {:?}",
        get_relationships_time
    );
    println!("Get all task files ({}): {:?}", files.len(), get_files_time);
    println!(
        "Total query time: {:?}",
        get_completed_time + get_todo_time + get_relationships_time + get_files_time
    );

    // All individual queries should be fast (< 5ms each)
    assert!(
        get_completed_time.as_millis() < 5,
        "Get completed IDs too slow: {:?}",
        get_completed_time
    );
    assert!(
        get_todo_time.as_millis() < 5,
        "Get todo tasks too slow: {:?}",
        get_todo_time
    );
    assert!(
        get_relationships_time.as_millis() < 10,
        "Get relationships too slow: {:?}",
        get_relationships_time
    );
    assert!(
        get_files_time.as_millis() < 5,
        "Get files too slow: {:?}",
        get_files_time
    );
}

#[test]
fn test_verify_indexes_used() {
    let (_temp_dir, conn) = create_benchmark_db();

    // Test EXPLAIN QUERY PLAN for each key query to verify indexes are used

    println!("\n=== Query Plan Analysis ===");

    // 1. Get completed task IDs
    let plan: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT id FROM tasks WHERE status IN ('done', 'irrelevant')",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    println!("\n1. Get completed IDs:");
    for line in &plan {
        println!("   {}", line);
    }

    // 2. Get todo tasks ordered by priority
    let plan: Vec<String> = {
        let mut stmt = conn
            .prepare("EXPLAIN QUERY PLAN SELECT id, title, priority FROM tasks WHERE status = 'todo' ORDER BY priority ASC")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    println!("\n2. Get todo tasks ordered by priority:");
    for line in &plan {
        println!("   {}", line);
    }

    // 3. Get relationships by type
    let plan: Vec<String> = {
        let mut stmt = conn
            .prepare("EXPLAIN QUERY PLAN SELECT task_id, related_id FROM task_relationships WHERE rel_type = 'dependsOn'")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    println!("\n3. Get relationships by type:");
    for line in &plan {
        println!("   {}", line);
    }

    // 4. Get all task files
    let plan: Vec<String> = {
        let mut stmt = conn
            .prepare("EXPLAIN QUERY PLAN SELECT task_id, file_path FROM task_files")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    println!("\n4. Get all task files:");
    for line in &plan {
        println!("   {}", line);
    }

    // Check if covering index is used for relationships query
    let plan_rel: Vec<String> = {
        let mut stmt = conn
            .prepare("EXPLAIN QUERY PLAN SELECT task_id, related_id FROM task_relationships WHERE rel_type = 'dependsOn'")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };

    let uses_covering_index = plan_rel
        .iter()
        .any(|p| p.contains("idx_task_relationships_type_taskid"));
    println!(
        "\nUses covering index for relationships: {}",
        uses_covering_index
    );

    // The covering index should be preferred since it contains all columns needed
    assert!(
        uses_covering_index,
        "Relationships query should use the covering index idx_task_relationships_type_taskid"
    );
}

#[test]
fn test_benchmark_worst_case_all_todo() {
    // Create a database where all 200 tasks are in 'todo' status
    // This is the worst case for task selection
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let conn = open_connection(temp_dir.path()).expect("Failed to open connection");
    create_schema(&conn).expect("Failed to create schema");

    // Create 200 todo tasks
    for i in 0..200 {
        let id = format!("US-{:03}", i);
        let title = format!("Task {} - all todo", i);
        let priority = (i % 100) as i32 + 1;
        insert_task(&conn, &id, &title, "todo", priority);

        // Add some files
        insert_task_file(&conn, &id, &format!("src/file_{}.rs", i % 20));
    }

    // Add complex dependency chains
    for i in 1..200 {
        let task_id = format!("US-{:03}", i);
        let dep_id = format!("US-{:03}", i - 1);
        insert_relationship(&conn, &task_id, &dep_id, "dependsOn");
    }

    drop(conn);

    // Measure selection time (includes connection open)
    let start = Instant::now();
    let conn = open_connection(temp_dir.path()).expect("Failed to open connection");
    let result =
        task_mgr::commands::next::select_next_task(&conn, &["src/file_0.rs".to_string()], &[]);
    let elapsed = start.elapsed();

    println!("\n=== Worst Case Benchmark (200 todo tasks, chain dependencies) ===");
    println!("Selection time: {:?}", elapsed);

    assert!(result.is_ok());
    let selection = result.unwrap();

    // Only US-000 should be eligible (it's the head of the dependency chain)
    println!("Eligible tasks: {}", selection.eligible_count);
    assert_eq!(
        selection.eligible_count, 1,
        "Only head of chain should be eligible"
    );

    // Should still be fast
    assert!(
        elapsed.as_millis() < 50,
        "Worst case selection time {:?} exceeds 50ms target",
        elapsed
    );
}

#[test]
fn test_benchmark_many_files_overlap() {
    // Test performance when there are many file overlaps to check
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let conn = open_connection(temp_dir.path()).expect("Failed to open connection");
    create_schema(&conn).expect("Failed to create schema");

    // Create 200 todo tasks with many files each
    for i in 0..200 {
        let id = format!("US-{:03}", i);
        insert_task(&conn, &id, &format!("Task {}", i), "todo", (i + 1) as i32);

        // Each task touches 10 files
        for f in 0..10 {
            insert_task_file(&conn, &id, &format!("src/module_{}/file_{}.rs", i % 20, f));
        }
    }

    drop(conn);

    // Test with large after_files list
    let after_files: Vec<String> = (0..50)
        .map(|f| format!("src/module_0/file_{}.rs", f % 10))
        .collect();

    let start = Instant::now();
    let conn = open_connection(temp_dir.path()).expect("Failed to open connection");
    let result = task_mgr::commands::next::select_next_task(&conn, &after_files, &[]);
    let elapsed = start.elapsed();

    println!("\n=== Many Files Overlap Benchmark ===");
    println!(
        "Tasks: 200, Files per task: 10, After files: {}",
        after_files.len()
    );
    println!("Selection time: {:?}", elapsed);

    assert!(result.is_ok());
    assert!(
        elapsed.as_millis() < 50,
        "Selection time {:?} exceeds 50ms target",
        elapsed
    );
}
