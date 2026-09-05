//! SQLite-backed canonical memory repository (M1B1).
//!
//! This implementation uses the canonical
//! [`apeireth_storage::SqliteConnectionPool`]. It never opens a `rusqlite`
//! connection, creates a pool, or spawns a writer thread on its own.

use std::path::Path;

use apeireth_core::kernel::Timestamp;
use apeireth_storage::{run_migrations, SqliteConnectionPool};
use async_trait::async_trait;
use rusqlite::{params, OptionalExtension, Row};

use super::domain::{MemoryId, MemoryItem};
use super::error::MemoryError;
use super::repository::{MemoryFilter, MemoryRepository};
use super::vector::{VectorMetadataStore, VectorRecord};

const SELECT_COLUMNS: &str = "id, data, importance, access_count, access_times, \
     created_at, valid_from, valid_until, is_tombstone, artifact_sig";

/// SQLite-backed implementation of the canonical memory repository.
#[derive(Debug)]
pub struct SqliteMemoryRepository {
    pool: SqliteConnectionPool,
}

impl SqliteMemoryRepository {
    /// Opens a file-backed repository, applying all pending storage
    /// migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let pool = SqliteConnectionPool::open(path).await?;
        Self::migrate(pool).await
    }

    /// Opens a shared in-memory repository, applying all pending storage
    /// migrations.
    pub async fn in_memory() -> Result<Self, MemoryError> {
        let pool = SqliteConnectionPool::in_memory().await?;
        Self::migrate(pool).await
    }

    /// Wraps an existing canonical pool without applying migrations.
    ///
    /// This is intended for callers that already own migration policy.
    pub fn from_pool(pool: SqliteConnectionPool) -> Self {
        Self { pool }
    }

    /// Borrows the underlying canonical pool.
    pub fn pool(&self) -> &SqliteConnectionPool {
        &self.pool
    }

    async fn migrate(pool: SqliteConnectionPool) -> Result<Self, MemoryError> {
        pool.write(|conn| run_migrations(conn)).await?;
        Ok(Self { pool })
    }
}

/// Raw persisted row, converted into a [`MemoryItem`] only after leaving the
/// storage closure. That keeps domain validation errors in the memory error
/// space instead of being forced through `rusqlite::Error`.
struct MemoryRow {
    id: String,
    data: String,
    importance: f64,
    access_count: i64,
    access_times_json: String,
    created_at_ms: i64,
    valid_from_ms: i64,
    valid_until_ms: Option<i64>,
    is_tombstone: i64,
    artifact_sig: Option<String>,
}

impl MemoryRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            data: row.get(1)?,
            importance: row.get(2)?,
            access_count: row.get(3)?,
            access_times_json: row.get(4)?,
            created_at_ms: row.get(5)?,
            valid_from_ms: row.get(6)?,
            valid_until_ms: row.get(7)?,
            is_tombstone: row.get(8)?,
            artifact_sig: row.get(9)?,
        })
    }

    fn into_item(self) -> Result<MemoryItem, MemoryError> {
        let id = MemoryId::from_validated(self.id);
        let access_count = u32::try_from(self.access_count).map_err(|_| {
            MemoryError::InvalidData(format!(
                "persisted access_count {} does not fit u32",
                self.access_count
            ))
        })?;
        let access_times = parse_access_times(&self.access_times_json)?;
        let created_at = timestamp_from_millis(self.created_at_ms, "created_at")?;
        let valid_from = timestamp_from_millis(self.valid_from_ms, "valid_from")?;
        let valid_until = match self.valid_until_ms {
            Some(ms) => Some(timestamp_from_millis(ms, "valid_until")?),
            None => None,
        };

        Ok(MemoryItem {
            id,
            data: self.data,
            importance: self.importance,
            access_count,
            access_times,
            created_at,
            valid_from,
            valid_until,
            is_tombstone: self.is_tombstone != 0,
            artifact_sig: self.artifact_sig,
        })
    }
}

fn timestamp_from_millis(ms: i64, field: &'static str) -> Result<Timestamp, MemoryError> {
    Timestamp::from_epoch_millis(ms)
        .ok_or_else(|| MemoryError::InvalidData(format!("persisted {field} is out of range: {ms}")))
}

fn access_times_to_json(times: &[Timestamp]) -> Result<String, MemoryError> {
    let millis: Vec<i64> = times.iter().map(Timestamp::epoch_millis).collect();
    serde_json::to_string(&millis)
        .map_err(|e| MemoryError::InvalidData(format!("failed to serialize access_times: {e}")))
}

fn parse_access_times(json: &str) -> Result<Vec<Timestamp>, MemoryError> {
    let millis: Vec<i64> = serde_json::from_str(json)
        .map_err(|e| MemoryError::InvalidData(format!("persisted access_times is invalid: {e}")))?;
    millis
        .into_iter()
        .map(|ms| timestamp_from_millis(ms, "access_times"))
        .collect()
}

#[async_trait]
impl MemoryRepository for SqliteMemoryRepository {
    async fn insert(&self, item: MemoryItem) -> Result<(), MemoryError> {
        item.validate()?;
        let access_times_json = access_times_to_json(&item.access_times)?;
        let id = item.id.as_str().to_string();
        let data = item.data;
        let importance = item.importance;
        let access_count = i64::from(item.access_count);
        let created_at_ms = item.created_at.epoch_millis();
        let valid_from_ms = item.valid_from.epoch_millis();
        let valid_until_ms = item.valid_until.map(|ts| ts.epoch_millis());
        let is_tombstone = i64::from(item.is_tombstone);
        let artifact_sig = item.artifact_sig;

        let inserted = self
            .pool
            .write(move |conn| {
                Ok(conn.execute(
                    "INSERT INTO memory_items \
                     (id, data, importance, access_count, access_times, created_at, \
                      valid_from, valid_until, is_tombstone, artifact_sig) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                     ON CONFLICT(id) DO NOTHING",
                    params![
                        id,
                        data,
                        importance,
                        access_count,
                        access_times_json,
                        created_at_ms,
                        valid_from_ms,
                        valid_until_ms,
                        is_tombstone,
                        artifact_sig
                    ],
                )?)
            })
            .await?;

        if inserted == 0 {
            return Err(MemoryError::Conflict(format!(
                "memory item already exists: {}",
                item.id
            )));
        }
        Ok(())
    }

    async fn get(&self, id: &MemoryId) -> Result<Option<MemoryItem>, MemoryError> {
        let id = id.as_str().to_string();
        let row = self.pool.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT id, data, importance, access_count, access_times, \
                         created_at, valid_from, valid_until, is_tombstone, artifact_sig \
                         FROM memory_items WHERE id = ?1 AND is_tombstone = 0",
                    params![id],
                    MemoryRow::from_row,
                )
                .optional()?)
        })?;

        // `get` is a normal read, so tombstoned rows are excluded in SQL and
        // this branch only sees non-tombstoned items.
        match row {
            Some(row) => row.into_item().map(Some),
            None => Ok(None),
        }
    }

    async fn update(&self, item: MemoryItem) -> Result<(), MemoryError> {
        item.validate()?;
        let access_times_json = access_times_to_json(&item.access_times)?;
        let id = item.id.as_str().to_string();
        let data = item.data;
        let importance = item.importance;
        let access_count = i64::from(item.access_count);
        let created_at_ms = item.created_at.epoch_millis();
        let valid_from_ms = item.valid_from.epoch_millis();
        let valid_until_ms = item.valid_until.map(|ts| ts.epoch_millis());
        let is_tombstone = i64::from(item.is_tombstone);
        let artifact_sig = item.artifact_sig;

        let updated = self
            .pool
            .write(move |conn| {
                Ok(conn.execute(
                    "UPDATE memory_items SET \
                     data = ?2, importance = ?3, access_count = ?4, access_times = ?5, \
                     created_at = ?6, valid_from = ?7, valid_until = ?8, \
                     is_tombstone = ?9, artifact_sig = ?10 \
                     WHERE id = ?1",
                    params![
                        id,
                        data,
                        importance,
                        access_count,
                        access_times_json,
                        created_at_ms,
                        valid_from_ms,
                        valid_until_ms,
                        is_tombstone,
                        artifact_sig
                    ],
                )?)
            })
            .await?;

        if updated == 0 {
            return Err(MemoryError::NotFound(format!(
                "memory item not found: {}",
                item.id
            )));
        }
        Ok(())
    }

    async fn query(&self, filter: &MemoryFilter) -> Result<Vec<MemoryItem>, MemoryError> {
        let as_of_ms = filter.as_of.epoch_millis();
        let include_tombstones = filter.include_tombstones;
        let mut sql = format!(
            "SELECT {SELECT_COLUMNS} FROM memory_items \
             WHERE valid_from <= ?1 AND (valid_until IS NULL OR valid_until > ?1)"
        );
        if !include_tombstones {
            sql.push_str(" AND is_tombstone = 0");
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");

        let limit = filter
            .limit
            .and_then(|n| i64::try_from(n).ok())
            .unwrap_or(-1);

        let rows = if limit >= 0 {
            let limit_for_sql = limit;
            self.pool.read(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(params![as_of_ms, limit_for_sql])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(MemoryRow::from_row(row)?);
                }
                Ok(out)
            })?
        } else {
            self.pool.read(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(params![as_of_ms])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(MemoryRow::from_row(row)?);
                }
                Ok(out)
            })?
        };

        rows.into_iter()
            .map(MemoryRow::into_item)
            .collect::<Result<Vec<_>, _>>()
    }

    async fn tombstone(&self, id: &MemoryId) -> Result<(), MemoryError> {
        let id = id.as_str().to_string();

        let state: Option<i64> = {
            let id_for_read = id.clone();
            self.pool.read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT is_tombstone FROM memory_items WHERE id = ?1",
                        params![id_for_read],
                        |row| row.get(0),
                    )
                    .optional()?)
            })?
        };

        match state {
            None => Err(MemoryError::NotFound(format!(
                "memory item not found: {id}"
            ))),
            Some(1) => Ok(()),
            Some(_) => {
                let id_for_write = id;
                let updated = self
                    .pool
                    .write(move |conn| {
                        Ok(conn.execute(
                            "UPDATE memory_items SET is_tombstone = 1 \
                             WHERE id = ?1 AND is_tombstone = 0",
                            params![id_for_write],
                        )?)
                    })
                    .await?;

                if updated == 0 {
                    // Lost a race with another tombstone; semantics are
                    // idempotent so this is success rather than an error.
                    return Ok(());
                }
                Ok(())
            }
        }
    }
}

#[async_trait]
impl VectorMetadataStore for SqliteMemoryRepository {
    async fn get_vector(&self, memory_id: &MemoryId) -> Result<Option<VectorRecord>, MemoryError> {
        let id = memory_id.as_str().to_string();
        let row = self.pool.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT memory_id, model_id, dimension, vector_json, content_hash, updated_at \
                         FROM memory_vectors WHERE memory_id = ?1",
                    params![id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()?)
        })?;
        let Some((id, model_id, dimension, vector_json, content_hash, updated_at)) = row else {
            return Ok(None);
        };
        let memory_id = MemoryId::new(id).map_err(|error| error)?;
        let vector = serde_json::from_str(&vector_json).map_err(|error| {
            MemoryError::InvalidData(format!("invalid persisted vector: {error}"))
        })?;
        let dimension = usize::try_from(dimension).map_err(|_| {
            MemoryError::InvalidData("persisted vector dimension is negative".into())
        })?;
        let updated_at = Timestamp::from_epoch_millis(updated_at).ok_or_else(|| {
            MemoryError::InvalidData("persisted vector timestamp out of range".into())
        })?;
        Ok(Some(VectorRecord {
            memory_id,
            model_id,
            dimension,
            vector,
            content_hash,
            updated_at,
        }))
    }

    async fn upsert_vector(&self, record: VectorRecord) -> Result<(), MemoryError> {
        record.validate_compatible(&record.model_id, record.dimension)?;
        let vector_json = serde_json::to_string(&record.vector).map_err(|error| {
            MemoryError::InvalidData(format!("invalid vector payload: {error}"))
        })?;
        let memory_id = record.memory_id.to_string();
        let model_id = record.model_id;
        let dimension = i64::try_from(record.dimension)
            .map_err(|_| MemoryError::InvalidData("vector dimension does not fit i64".into()))?;
        let content_hash = record.content_hash;
        let updated_at = record.updated_at.epoch_millis();
        self.pool
            .write(move |conn| {
                conn.execute(
                    "INSERT INTO memory_vectors \
                     (memory_id, model_id, dimension, vector_json, content_hash, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(memory_id) DO UPDATE SET \
                     model_id=excluded.model_id, dimension=excluded.dimension, \
                     vector_json=excluded.vector_json, content_hash=excluded.content_hash, \
                     updated_at=excluded.updated_at",
                    params![
                        memory_id,
                        model_id,
                        dimension,
                        vector_json,
                        content_hash,
                        updated_at
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn remove_vector(&self, memory_id: &MemoryId) -> Result<(), MemoryError> {
        let id = memory_id.as_str().to_string();
        self.pool
            .write(move |conn| {
                conn.execute(
                    "DELETE FROM memory_vectors WHERE memory_id = ?1",
                    params![id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}
