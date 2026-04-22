//! Migration 18: Create dedup_dismissals table for persisting curate dedup pair judgements.
//!
//! ## Changes
//! - Creates `dedup_dismissals` table with composite PK (id_lo, id_hi):
//!   - `id_lo INTEGER NOT NULL` — lower of the two learning IDs
//!   - `id_hi INTEGER NOT NULL` — higher of the two learning IDs
//!   - `CHECK (id_lo < id_hi)` — schema-level enforcement of the ordering invariant
//!     (defense-in-depth; `normalize_pair` enforces it on the Rust side too).
//! - `idx_dedup_dismissals_hi` index speeds up lookups by the high-side ID.
//!
//! ## Semantics
//! - A row (id_lo, id_hi) means the LLM examined this pair and found them distinct.
//! - `curate dedup` skips clusters where all C(N,2) pairs are already dismissed.
//! - No foreign keys to learnings — dismissed pairs for retired learnings are inert.

use super::Migration;

/// Migration 18: Create dedup_dismissals table.
pub static MIGRATION: Migration = Migration {
    version: 18,
    description: "Create dedup_dismissals table for persisting curate dedup pair judgements",
    up_sql: r#"
        CREATE TABLE dedup_dismissals (
            id_lo INTEGER NOT NULL,
            id_hi INTEGER NOT NULL,
            PRIMARY KEY (id_lo, id_hi),
            CHECK (id_lo < id_hi)
        );
        CREATE INDEX idx_dedup_dismissals_hi ON dedup_dismissals(id_hi);
        UPDATE global_state SET schema_version = 18 WHERE id = 1;
    "#,
    down_sql: r#"
        DROP INDEX IF EXISTS idx_dedup_dismissals_hi;
        DROP TABLE IF EXISTS dedup_dismissals;
        UPDATE global_state SET schema_version = 17 WHERE id = 1;
    "#,
};

#[cfg(test)]
mod tests {
    use crate::db::migrations::{
        CURRENT_SCHEMA_VERSION, MIGRATIONS, get_schema_version, run_migrations,
    };
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_migrated_db() -> (TempDir, rusqlite::Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    /// AC: CURRENT_SCHEMA_VERSION >= 18 after full migration run.
    #[test]
    fn test_v18_current_schema_version() {
        const _: () = assert!(
            CURRENT_SCHEMA_VERSION >= 18,
            "CURRENT_SCHEMA_VERSION must be at least 18"
        );
        let (_tmp, conn) = setup_migrated_db();
        let version = get_schema_version(&conn).unwrap();
        assert!(
            version >= 18,
            "DB schema_version must be >= 18 after running migrations, got {version}"
        );
    }

    /// AC: dedup_dismissals table and its columns exist after v18 migration.
    #[test]
    fn test_v18_dedup_dismissals_table_exists() {
        let (_tmp, conn) = setup_migrated_db();
        for col in ["id_lo", "id_hi"] {
            let exists: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) > 0 FROM pragma_table_info('dedup_dismissals') WHERE name = '{col}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                exists,
                "dedup_dismissals.{col} column must exist after v18 migration"
            );
        }
    }

    /// AC: idx_dedup_dismissals_hi index exists after v18 migration.
    #[test]
    fn test_v18_index_exists() {
        let (_tmp, conn) = setup_migrated_db();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_dedup_dismissals_hi'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "idx_dedup_dismissals_hi must exist after v18 migration"
        );
    }

    /// AC: CHECK (id_lo < id_hi) rejects equal and reversed tuples.
    #[test]
    fn test_v18_check_constraint_rejects_bad_tuples() {
        let (_tmp, conn) = setup_migrated_db();

        let self_pair = conn
            .execute(
                "INSERT INTO dedup_dismissals (id_lo, id_hi) VALUES (5, 5)",
                [],
            )
            .expect_err("(5, 5) must violate CHECK (id_lo < id_hi)");
        assert!(
            self_pair.to_string().to_ascii_uppercase().contains("CHECK"),
            "error for (5, 5) should name CHECK constraint, got: {self_pair}"
        );

        let reversed = conn
            .execute(
                "INSERT INTO dedup_dismissals (id_lo, id_hi) VALUES (10, 5)",
                [],
            )
            .expect_err("(10, 5) must violate CHECK (id_lo < id_hi)");
        assert!(
            reversed.to_string().to_ascii_uppercase().contains("CHECK"),
            "error for (10, 5) should name CHECK constraint, got: {reversed}"
        );

        // Well-ordered tuple still inserts.
        conn.execute(
            "INSERT INTO dedup_dismissals (id_lo, id_hi) VALUES (3, 7)",
            [],
        )
        .expect("(3, 7) must satisfy CHECK and insert");
    }

    /// AC: v18 down migration drops the table and index, reverts schema_version to 17.
    #[test]
    fn test_v18_migration_down() {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();

        let v18 = MIGRATIONS.iter().find(|m| m.version == 18).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute_batch(v18.down_sql).unwrap();
        tx.commit().unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(
            version, 17,
            "schema_version must revert to 17 after v18 down migration"
        );

        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='dedup_dismissals'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !table_exists,
            "dedup_dismissals table must be removed after v18 down migration"
        );

        let idx_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_dedup_dismissals_hi'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !idx_exists,
            "idx_dedup_dismissals_hi must be removed after v18 down migration"
        );
    }
}
