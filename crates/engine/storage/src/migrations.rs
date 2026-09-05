//! Versioned schema migrations.
//!
//! The migration engine records the applied schema version in SQLite's
//! `PRAGMA user_version`. Each migration runs inside a transaction; a failed
//! migration rolls back before the version is marked as applied.

use rusqlite::Connection;

use crate::StorageError;

/// The latest schema version this storage foundation knows about.
pub const LATEST_SCHEMA_VERSION: i64 = 3;

/// One schema migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// Monotonic schema version.
    pub version: i64,
    /// Short migration name.
    pub name: &'static str,
    /// SQL statements applied by the migration.
    pub sql: &'static str,
}

/// Ordered migration list.
///
/// Version 1 preserves the donor `reconstruction_v2` on-disk table set so an
/// existing Apeireth 2.0 database created by the parallel implementation can
/// be opened without data loss. The SQL is intentionally `IF NOT EXISTS`:
/// donor databases may already contain these tables while still carrying
/// `user_version = 0`, so the first migration must be idempotent.
static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_storage_schema",
        sql: r#"
CREATE TABLE IF NOT EXISTS episodes (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS notes (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS agent_traces (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS facts (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS links (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS topic_groups (id TEXT PRIMARY KEY, data TEXT);
CREATE TABLE IF NOT EXISTS provenance (id TEXT PRIMARY KEY, data TEXT);
CREATE INDEX IF NOT EXISTS idx_facts_id ON facts(id);
"#,
    },
    Migration {
        version: 2,
        name: "canonical_memory_items",
        sql: r#"
CREATE TABLE IF NOT EXISTS memory_items (
    id              TEXT PRIMARY KEY,
    data            TEXT NOT NULL,
    importance      REAL NOT NULL,
    access_count    INTEGER NOT NULL,
    access_times    TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    valid_from      INTEGER NOT NULL,
    valid_until     INTEGER,
    is_tombstone    INTEGER NOT NULL,
    artifact_sig    TEXT
);
CREATE INDEX IF NOT EXISTS idx_memory_items_created_at ON memory_items(created_at);
CREATE INDEX IF NOT EXISTS idx_memory_items_valid_from ON memory_items(valid_from);
CREATE INDEX IF NOT EXISTS idx_memory_items_valid_until ON memory_items(valid_until);
CREATE INDEX IF NOT EXISTS idx_memory_items_tombstone ON memory_items(is_tombstone);
"#,
    },
    Migration {
        version: 3,
        name: "canonical_vector_metadata",
        sql: r#"
CREATE TABLE IF NOT EXISTS memory_vectors (
    memory_id TEXT PRIMARY KEY,
    model_id TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    vector_json TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_vectors_model_dimension
    ON memory_vectors(model_id, dimension);
"#,
    },
];

/// Returns the current schema version recorded in `PRAGMA user_version`.
pub fn current_version(conn: &Connection) -> Result<i64, StorageError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StorageError::from)
}

/// Applies all pending migrations.
pub fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    let current = current_version(conn)?;

    for migration in MIGRATIONS {
        if migration.version <= current {
            continue;
        }
        run_one(conn, migration)?;
    }

    Ok(())
}

fn run_one(conn: &Connection, migration: &Migration) -> Result<(), StorageError> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| migration_error(migration, e.to_string()))?;

    let result = (|| -> Result<(), StorageError> {
        conn.execute_batch(migration.sql)
            .map_err(|e| migration_error(migration, e.to_string()))?;
        set_version(conn, migration.version, migration.name)?;
        conn.execute_batch("COMMIT")
            .map_err(|e| migration_error(migration, e.to_string()))
    })();

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn set_version(conn: &Connection, version: i64, name: &'static str) -> Result<(), StorageError> {
    conn.pragma_update(None, "user_version", version)
        .map_err(|e| StorageError::migration(version, name, e.to_string()))
}

fn migration_error(migration: &Migration, message: String) -> StorageError {
    StorageError::migration(migration.version, migration.name, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::SqliteConnectionPool;

    #[tokio::test]
    async fn fresh_database_migrates_to_latest_version() {
        let pool = SqliteConnectionPool::in_memory().await.unwrap();

        pool.write(|conn| run_migrations(conn)).await.unwrap();

        let version = pool.read(current_version).unwrap();
        assert_eq!(version, LATEST_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let pool = SqliteConnectionPool::in_memory().await.unwrap();

        pool.write(|conn| run_migrations(conn)).await.unwrap();
        pool.write(|conn| run_migrations(conn)).await.unwrap();

        let version = pool.read(current_version).unwrap();
        assert_eq!(version, LATEST_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn fresh_database_migrates_through_v1_to_v3() {
        let pool = SqliteConnectionPool::in_memory().await.unwrap();

        pool.write(|conn| run_migrations(conn)).await.unwrap();

        let version = pool.read(current_version).unwrap();
        assert_eq!(version, LATEST_SCHEMA_VERSION);

        let facts_exists: bool = pool
            .read(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'facts')",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(facts_exists);

        let memory_items_exists: bool = pool
            .read(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_items')",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(memory_items_exists);

        let vectors_exists: bool = pool
            .read(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_vectors')",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(vectors_exists);
    }

    #[tokio::test]
    async fn existing_v1_database_upgrades_to_v2_without_data_loss() {
        let pool = SqliteConnectionPool::in_memory().await.unwrap();

        pool.write(|conn| {
            conn.execute_batch(MIGRATIONS[0].sql)?;
            conn.pragma_update(None, "user_version", 1)?;
            conn.execute(
                "INSERT INTO facts (id, data) VALUES ('f1', '{\"kept\":true}')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        pool.write(|conn| run_migrations(conn)).await.unwrap();

        let version = pool.read(current_version).unwrap();
        assert_eq!(version, LATEST_SCHEMA_VERSION);

        let facts_count: i64 = pool
            .read(|conn| Ok(conn.query_row("SELECT count(*) FROM facts", [], |row| row.get(0))?))
            .unwrap();
        assert_eq!(facts_count, 1);

        let memory_items_exists: bool = pool
            .read(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_items')",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(memory_items_exists);
    }

    #[tokio::test]
    async fn failed_migration_does_not_mark_version_complete() {
        let pool = SqliteConnectionPool::in_memory().await.unwrap();

        let bad = Migration {
            version: 99,
            name: "bad_test_migration",
            sql: "CREATE TABLE broken (id TEXT;", // intentionally invalid SQL
        };

        let result = pool.write(move |conn| run_one(conn, &bad)).await;
        assert!(result.is_err());

        let version = pool.read(current_version).unwrap();
        assert_eq!(version, 0);
    }
}
