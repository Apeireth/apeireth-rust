//! Core Capability Expansion Phase 3 — 记忆治理 (Memory Mutation / Forget / Protect).
//!
//! ## 设计: sidecar 治理表, 不破坏 append-only episodes
//! episodes 表由 trigger 强制 append-only (UPDATE/DELETE 一律 ABORT). 记忆治理**不**改
//! 原始 episode 行, 而是用独立 `episode_governance` 表 (V6) 记录可变元数据:
//! - `status`: active / forgotten (软删). forgotten → 从默认检索排除.
//! - `protected`: 防自动遗忘/压缩 (需显式解除才能 forget).
//! - `content_override`: 用户修订内容 (update). 原始 content 保留不动 → provenance 完整.
//! - `revision`: 乐观并发 (expected_rev CAS).
//!
//! 原始 episode 行 + governance sidecar = 完整记忆. 存量行无 governance 记录 → 默认
//! active/unprotected (LEFT JOIN NULL 语义, 零数据迁移).
//!
//! ## Forget != Purge
//! - `forget` = 软删 (status=forgotten): 从默认检索/UI 排除, 但保留最小审计事实
//!   (episode_id, forgotten_at, reason). episode 原始行不删 → 引用完整性不破.
//! - `purge` (真正物理删除) 本轮**不**实现 (需明确 cascading/retention).
//!
//! ## Protect
//! protected episode 不被普通 forget 接受 (返回 Protected 错误); 需先 unprotect.
//! 防止自动压缩/反思误删重要记忆.
//!
//! ## Graph Integrity
//! 本轮 graph facts (`factg-*`) / links (`link-*`) 也存为 episodes. forget 一个 factg-* episode
//! 时, 该 fact 从 governed 检索排除 (LEFT JOIN 过滤). 不重建关系 (复杂度高, 留待后续),
//! 但不留 dangling pointer (link.from/to 指向的 episode 仍存在, 只是 forgotten 状态过滤).
//!
//! ## Provenance
//! update 不覆盖 id/timestamp/role/session_id/provenance (这些是不可变来源). 只 override content
//! + 记录 updated_at/updated_by. 原始 content 通过 governance.content_override IS NULL 仍可读回.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{MemoryError, MemoryResult, SqliteMemoryStore};
use apeireth_core::kernel::memory::Episode;

/// 记忆治理状态.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryGovernanceStatus {
    /// 活跃 (默认).
    Active,
    /// 已遗忘 (软删, 从默认检索排除).
    Forgotten,
}

impl MemoryGovernanceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Forgotten => "forgotten",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "forgotten" => Self::Forgotten,
            _ => Self::Active,
        }
    }
}

/// 治理后的 episode (原始 episode + 治理元数据).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernedEpisode {
    /// 原始 episode (content 可能被 override 替换; 原始 content 通过 governance 仍可查).
    #[serde(flatten)]
    pub episode: Episode,
    /// 治理状态.
    pub status: MemoryGovernanceStatus,
    /// 是否受保护.
    pub protected: bool,
    /// 内容修订 (Some = 用户编辑过; None = 原始内容).
    pub content_override: Option<String>,
    /// 修订版本号.
    pub revision: i64,
    pub updated_at: Option<i64>,
    pub updated_by: Option<String>,
    pub forgotten_at: Option<i64>,
}

/// 记忆治理错误.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryGovernanceError {
    NotFound(String),
    /// 已遗忘 (二次 forget / 对 forgotten episode 操作).
    AlreadyForgotten(String),
    /// 受保护, 拒绝普通 forget (需先 unprotect).
    Protected(String),
    /// 乐观并发冲突.
    Conflict {
        id: String,
        expected: i64,
        actual: i64,
    },
    Invalid(String),
}

impl std::fmt::Display for MemoryGovernanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "episode `{id}` not found"),
            Self::AlreadyForgotten(id) => write!(f, "episode `{id}` already forgotten"),
            Self::Protected(id) => write!(f, "episode `{id}` is protected (unprotect first)"),
            Self::Conflict {
                id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "episode `{id}` revision conflict: expected {expected}, actual {actual}"
                )
            }
            Self::Invalid(m) => write!(f, "invalid memory governance: {m}"),
        }
    }
}
impl std::error::Error for MemoryGovernanceError {}
impl From<MemoryGovernanceError> for MemoryError {
    fn from(e: MemoryGovernanceError) -> Self {
        MemoryError::Invalid(e.to_string())
    }
}
impl From<MemoryError> for MemoryGovernanceError {
    fn from(e: MemoryError) -> Self {
        Self::Invalid(e.to_string())
    }
}
impl From<rusqlite::Error> for MemoryGovernanceError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}

const MAX_CONTENT_LEN: usize = 100_000;

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 记忆治理存储接口.
pub trait MemoryGovernanceStore: Send + Sync {
    /// 读取单 episode 的治理视图 (含 override content).
    fn get_governed(
        &self,
        episode_id: &str,
    ) -> Result<Option<GovernedEpisode>, MemoryGovernanceError>;

    /// 更新内容 (修订). 乐观并发 expected_rev CAS. 不改原始 episode 行.
    fn update_episode_content(
        &self,
        episode_id: &str,
        new_content: &str,
        updated_by: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError>;

    /// 遗忘 (软删). protected episode 拒绝. forgotten episode 二次 forget 报 AlreadyForgotten.
    fn forget_episode(
        &self,
        episode_id: &str,
        reason: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError>;

    /// 保护 (防自动遗忘/压缩).
    fn protect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError>;

    /// 解除保护.
    fn unprotect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError>;

    /// 治理检索某 session 最近 N 条 (排除 forgotten, 应用 override). 对话检索主路径.
    fn governed_recent_episodes(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError>;

    /// 治理复合查询 (排除 forgotten, 应用 override).
    fn governed_query(
        &self,
        q: &crate::EpisodeQuery,
    ) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError>;
}

pub(crate) const GOVERNED_COLS: &'static str =
    "e.id AS id, e.timestamp AS timestamp, e.role AS role, \
     e.content AS content, e.session_id AS session_id, \
     g.status AS status, g.protected AS protected, g.content_override AS content_override, \
     g.revision AS revision, g.updated_at AS updated_at, g.updated_by AS updated_by, \
     g.forgotten_at AS forgotten_at";

pub(crate) fn read_governed_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GovernedEpisode> {
    let id: String = row.get("id")?;
    let timestamp: i64 = row.get("timestamp")?;
    let role: String = row.get("role")?;
    let orig_content: String = row.get("content")?;
    let session_id: String = row.get("session_id")?;
    let override_content: Option<String> = row.get("content_override")?;
    let status_str: Option<String> = row.get("status")?;
    let protected: Option<i64> = row.get("protected")?;
    let revision: Option<i64> = row.get("revision")?;
    let updated_at: Option<i64> = row.get("updated_at")?;
    let updated_by: Option<String> = row.get("updated_by")?;
    let forgotten_at: Option<i64> = row.get("forgotten_at")?;
    let content = override_content.unwrap_or(orig_content);
    Ok(GovernedEpisode {
        episode: Episode {
            id,
            timestamp,
            role,
            content,
            session_id,
        },
        status: MemoryGovernanceStatus::from_str(status_str.as_deref().unwrap_or("active")),
        protected: protected.map_or(false, |p| p != 0),
        content_override: row.get("content_override")?,
        revision: revision.unwrap_or(0),
        updated_at,
        updated_by,
        forgotten_at,
    })
}

pub(crate) fn ensure_governance_on_conn(
    conn: &rusqlite::Connection,
    episode_id: &str,
) -> Result<(MemoryGovernanceStatus, bool, i64), MemoryGovernanceError> {
    conn.execute(
        "INSERT OR IGNORE INTO episode_governance (episode_id, status, protected, revision) \
         VALUES (?1, 'active', 0, 0)",
        params![episode_id],
    )?;
    let row: (String, i64, i64) = conn
        .query_row(
            "SELECT status, protected, revision FROM episode_governance WHERE episode_id = ?1",
            params![episode_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    Ok((MemoryGovernanceStatus::from_str(&row.0), row.1 != 0, row.2))
}

pub(crate) fn fetch_governed_on_conn(
    conn: &rusqlite::Connection,
    episode_id: &str,
) -> Result<Option<GovernedEpisode>, MemoryGovernanceError> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {} FROM episodes e \
                 LEFT JOIN episode_governance g ON g.episode_id = e.id \
                 WHERE e.id = ?1",
                GOVERNED_COLS
            ),
            params![episode_id],
            read_governed_row,
        )
        .optional()
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    Ok(row)
}

pub(crate) fn cas_failure_on_conn(
    conn: &rusqlite::Connection,
    episode_id: &str,
    expected_rev: i64,
) -> MemoryGovernanceError {
    match fetch_governed_on_conn(conn, episode_id) {
        Ok(Some(g)) => MemoryGovernanceError::Conflict {
            id: episode_id.to_string(),
            expected: expected_rev,
            actual: g.revision,
        },
        _ => MemoryGovernanceError::NotFound(episode_id.to_string()),
    }
}

pub(crate) fn get_governed_impl(
    conn: &rusqlite::Connection,
    episode_id: &str,
) -> Result<Option<GovernedEpisode>, MemoryGovernanceError> {
    if episode_id.trim().is_empty() {
        return Err(MemoryGovernanceError::Invalid("episode id is empty".into()));
    }
    fetch_governed_on_conn(conn, episode_id)
}

pub(crate) fn update_episode_content_impl(
    conn: &rusqlite::Connection,
    episode_id: &str,
    new_content: &str,
    updated_by: Option<&str>,
    expected_rev: i64,
) -> Result<GovernedEpisode, MemoryGovernanceError> {
    if episode_id.trim().is_empty() {
        return Err(MemoryGovernanceError::Invalid("episode id is empty".into()));
    }
    if new_content.chars().count() > MAX_CONTENT_LEN {
        return Err(MemoryGovernanceError::Invalid(format!(
            "content too long (max {MAX_CONTENT_LEN} chars)"
        )));
    }
    if fetch_governed_on_conn(conn, episode_id)?.is_none() {
        return Err(MemoryGovernanceError::NotFound(episode_id.to_string()));
    }
    ensure_governance_on_conn(conn, episode_id)?;
    let now = now_ms();
    let updated = conn
        .execute(
            "UPDATE episode_governance SET content_override = ?1, revision = revision + 1, \
             updated_at = ?2, updated_by = ?3 \
             WHERE episode_id = ?4 AND revision = ?5",
            params![new_content, now, updated_by, episode_id, expected_rev],
        )
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    if updated == 0 {
        return Err(cas_failure_on_conn(conn, episode_id, expected_rev));
    }
    fetch_governed_on_conn(conn, episode_id)?
        .ok_or(MemoryGovernanceError::NotFound(episode_id.to_string()))
}

pub(crate) fn forget_episode_impl(
    conn: &rusqlite::Connection,
    episode_id: &str,
    reason: Option<&str>,
    expected_rev: i64,
) -> Result<GovernedEpisode, MemoryGovernanceError> {
    if episode_id.trim().is_empty() {
        return Err(MemoryGovernanceError::Invalid("episode id is empty".into()));
    }
    if fetch_governed_on_conn(conn, episode_id)?.is_none() {
        return Err(MemoryGovernanceError::NotFound(episode_id.to_string()));
    }
    let (status, protected, _rev) = ensure_governance_on_conn(conn, episode_id)?;
    if protected {
        return Err(MemoryGovernanceError::Protected(episode_id.to_string()));
    }
    if status == MemoryGovernanceStatus::Forgotten {
        return Err(MemoryGovernanceError::AlreadyForgotten(
            episode_id.to_string(),
        ));
    }
    let now = now_ms();
    let updated = conn
        .execute(
            "UPDATE episode_governance SET status = 'forgotten', forgotten_at = ?1, reason = ?2, \
             revision = revision + 1, updated_at = ?1 \
             WHERE episode_id = ?3 AND revision = ?4 AND protected = 0 AND status = 'active'",
            params![now, reason, episode_id, expected_rev],
        )
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    if updated == 0 {
        return Err(cas_failure_on_conn(conn, episode_id, expected_rev));
    }
    fetch_governed_on_conn(conn, episode_id)?
        .ok_or(MemoryGovernanceError::NotFound(episode_id.to_string()))
}

pub(crate) fn protect_episode_impl(
    conn: &rusqlite::Connection,
    episode_id: &str,
    expected_rev: i64,
) -> Result<GovernedEpisode, MemoryGovernanceError> {
    if episode_id.trim().is_empty() {
        return Err(MemoryGovernanceError::Invalid("episode id is empty".into()));
    }
    if fetch_governed_on_conn(conn, episode_id)?.is_none() {
        return Err(MemoryGovernanceError::NotFound(episode_id.to_string()));
    }
    ensure_governance_on_conn(conn, episode_id)?;
    let now = now_ms();
    let updated = conn
        .execute(
            "UPDATE episode_governance SET protected = 1, revision = revision + 1, updated_at = ?1 \
             WHERE episode_id = ?2 AND revision = ?3",
            params![now, episode_id, expected_rev],
        )
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    if updated == 0 {
        return Err(cas_failure_on_conn(conn, episode_id, expected_rev));
    }
    fetch_governed_on_conn(conn, episode_id)?
        .ok_or(MemoryGovernanceError::NotFound(episode_id.to_string()))
}

pub(crate) fn unprotect_episode_impl(
    conn: &rusqlite::Connection,
    episode_id: &str,
    expected_rev: i64,
) -> Result<GovernedEpisode, MemoryGovernanceError> {
    if episode_id.trim().is_empty() {
        return Err(MemoryGovernanceError::Invalid("episode id is empty".into()));
    }
    if fetch_governed_on_conn(conn, episode_id)?.is_none() {
        return Err(MemoryGovernanceError::NotFound(episode_id.to_string()));
    }
    ensure_governance_on_conn(conn, episode_id)?;
    let now = now_ms();
    let updated = conn
        .execute(
            "UPDATE episode_governance SET protected = 0, revision = revision + 1, updated_at = ?1 \
             WHERE episode_id = ?2 AND revision = ?3",
            params![now, episode_id, expected_rev],
        )
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    if updated == 0 {
        return Err(cas_failure_on_conn(conn, episode_id, expected_rev));
    }
    fetch_governed_on_conn(conn, episode_id)?
        .ok_or(MemoryGovernanceError::NotFound(episode_id.to_string()))
}

pub(crate) fn governed_recent_episodes_impl(
    conn: &rusqlite::Connection,
    session_id: &str,
    n: usize,
) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {} FROM episodes e \
             LEFT JOIN episode_governance g ON g.episode_id = e.id \
             WHERE e.session_id = ?1 AND (g.status IS NULL OR g.status = 'active') \
             ORDER BY e.timestamp DESC, e.id DESC LIMIT ?2",
            GOVERNED_COLS
        ))
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    let rows = stmt
        .query_map(params![session_id, n as i64], read_governed_row)
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    let mut out: Vec<GovernedEpisode> = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    out.reverse();
    Ok(out)
}

pub(crate) fn governed_query_impl(
    conn: &rusqlite::Connection,
    q: &crate::EpisodeQuery,
) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError> {
    let mut sql = format!(
        "SELECT {} FROM episodes e \
         LEFT JOIN episode_governance g ON g.episode_id = e.id \
         WHERE (g.status IS NULL OR g.status = 'active')",
        GOVERNED_COLS
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = &q.session_id {
        sql.push_str(" AND e.session_id = ?");
        args.push(Box::new(s.clone()));
    }
    if let Some(c) = &q.continuity_id {
        sql.push_str(" AND e.continuity_id = ?");
        args.push(Box::new(c.clone()));
    }
    if let Some(since) = q.since {
        sql.push_str(" AND e.timestamp >= ?");
        args.push(Box::new(since));
    }
    if let Some(until) = q.until {
        sql.push_str(" AND e.timestamp <= ?");
        args.push(Box::new(until));
    }
    if let Some(role) = &q.role {
        sql.push_str(" AND e.role = ?");
        args.push(Box::new(role.clone()));
    }
    sql.push_str(" ORDER BY e.timestamp ASC, e.id ASC");
    if let Some(n) = q.limit {
        sql.push_str(&format!(" LIMIT {}", n));
    }
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), read_governed_row)
        .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?);
    }
    Ok(out)
}

impl SqliteMemoryStore {
    pub const GOVERNED_COLS: &'static str = GOVERNED_COLS;

    pub fn read_governed_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GovernedEpisode> {
        read_governed_row(row)
    }

    pub fn ensure_governance(
        &self,
        episode_id: &str,
    ) -> Result<(MemoryGovernanceStatus, bool, i64), MemoryGovernanceError> {
        let conn = self.conn()?;
        ensure_governance_on_conn(&conn, episode_id)
    }

    pub fn fetch_governed(
        &self,
        episode_id: &str,
    ) -> Result<Option<GovernedEpisode>, MemoryGovernanceError> {
        let conn = self.conn()?;
        fetch_governed_on_conn(&conn, episode_id)
    }

    pub fn governance_cas_failure(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> MemoryGovernanceError {
        let conn = match self.conn() {
            Ok(c) => c,
            Err(e) => return MemoryGovernanceError::Invalid(e.to_string()),
        };
        cas_failure_on_conn(&conn, episode_id, expected_rev)
    }
}

impl MemoryGovernanceStore for SqliteMemoryStore {
    fn get_governed(
        &self,
        episode_id: &str,
    ) -> Result<Option<GovernedEpisode>, MemoryGovernanceError> {
        let conn = self.conn()?;
        get_governed_impl(&conn, episode_id)
    }

    fn update_episode_content(
        &self,
        episode_id: &str,
        new_content: &str,
        updated_by: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        let conn = self.conn()?;
        update_episode_content_impl(&conn, episode_id, new_content, updated_by, expected_rev)
    }

    fn forget_episode(
        &self,
        episode_id: &str,
        reason: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        let conn = self.conn()?;
        forget_episode_impl(&conn, episode_id, reason, expected_rev)
    }

    fn protect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        let conn = self.conn()?;
        protect_episode_impl(&conn, episode_id, expected_rev)
    }

    fn unprotect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        let conn = self.conn()?;
        unprotect_episode_impl(&conn, episode_id, expected_rev)
    }

    fn governed_recent_episodes(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError> {
        let conn = self.conn()?;
        governed_recent_episodes_impl(&conn, session_id, n)
    }

    fn governed_query(
        &self,
        q: &crate::EpisodeQuery,
    ) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError> {
        let conn = self.conn()?;
        governed_query_impl(&conn, q)
    }
}

impl MemoryGovernanceStore for crate::backend::sqlite::SqliteBackend {
    fn get_governed(
        &self,
        episode_id: &str,
    ) -> Result<Option<GovernedEpisode>, MemoryGovernanceError> {
        self.pool()
            .read(|conn| Ok(get_governed_impl(conn, episode_id)))
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }

    fn update_episode_content(
        &self,
        episode_id: &str,
        new_content: &str,
        updated_by: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.pool()
            .read(|conn| {
                Ok(update_episode_content_impl(
                    conn,
                    episode_id,
                    new_content,
                    updated_by,
                    expected_rev,
                ))
            })
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }

    fn forget_episode(
        &self,
        episode_id: &str,
        reason: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.pool()
            .read(|conn| Ok(forget_episode_impl(conn, episode_id, reason, expected_rev)))
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }

    fn protect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.pool()
            .read(|conn| Ok(protect_episode_impl(conn, episode_id, expected_rev)))
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }

    fn unprotect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.pool()
            .read(|conn| Ok(unprotect_episode_impl(conn, episode_id, expected_rev)))
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }

    fn governed_recent_episodes(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError> {
        self.pool()
            .read(|conn| Ok(governed_recent_episodes_impl(conn, session_id, n)))
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }

    fn governed_query(
        &self,
        q: &crate::EpisodeQuery,
    ) -> Result<Vec<GovernedEpisode>, MemoryGovernanceError> {
        self.pool()
            .read(|conn| Ok(governed_query_impl(conn, q)))
            .map_err(|e| MemoryGovernanceError::Invalid(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EpisodeQuery, EpisodeStore, SqliteMemoryStore};

    fn store() -> SqliteMemoryStore {
        SqliteMemoryStore::open_in_memory().unwrap()
    }

    fn put(store: &SqliteMemoryStore, id: &str, session: &str, content: &str) {
        store
            .put_episode(&Episode {
                id: id.into(),
                timestamp: 1000,
                role: "user".into(),
                content: content.into(),
                session_id: session.into(),
            })
            .unwrap();
    }

    #[test]
    fn memory_update_content_override() {
        let s = store();
        put(&s, "ep-1", "me", "原始内容");
        let g = s
            .update_episode_content("ep-1", "修订内容", Some("owner"), 0)
            .unwrap();
        assert_eq!(g.episode.content, "修订内容");
        assert_eq!(g.content_override.as_deref(), Some("修订内容"));
        assert_eq!(g.revision, 1);
        assert_eq!(g.updated_by.as_deref(), Some("owner"));
        // 原始 episode 行未变 (append-only 不动) — 通过 get_episode 读原始 content.
        let orig = s.get_episode("ep-1").unwrap().unwrap();
        assert_eq!(orig.content, "原始内容");
    }

    #[test]
    fn memory_update_invalid_id_too_long() {
        let s = store();
        put(&s, "ep-1", "me", "x");
        let big = "a".repeat(MAX_CONTENT_LEN + 1);
        let err = s.update_episode_content("ep-1", &big, None, 0).unwrap_err();
        assert!(matches!(err, MemoryGovernanceError::Invalid(_)));
    }

    #[test]
    fn memory_forget_excludes_from_retrieval() {
        let s = store();
        put(&s, "ep-1", "me", "保留");
        put(&s, "ep-2", "me", "遗忘");
        s.forget_episode("ep-2", Some("不再相关"), 0).unwrap();
        // governed 检索排除 forgotten
        let recent = s.governed_recent_episodes("me", 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].episode.id, "ep-1");
        // 单读 forgotten 仍可取 (审计/恢复用)
        let g = s.get_governed("ep-2").unwrap().unwrap();
        assert_eq!(g.status, MemoryGovernanceStatus::Forgotten);
        assert!(g.forgotten_at.is_some());
    }

    #[test]
    fn memory_forget_twice_already_forgotten() {
        let s = store();
        put(&s, "ep-1", "me", "x");
        s.forget_episode("ep-1", None, 0).unwrap();
        let err = s.forget_episode("ep-1", None, 1).unwrap_err();
        assert!(matches!(err, MemoryGovernanceError::AlreadyForgotten(_)));
    }

    #[test]
    fn memory_protect_blocks_forget() {
        let s = store();
        put(&s, "ep-1", "me", "重要");
        s.protect_episode("ep-1", 0).unwrap();
        // protected → 普通 forget 拒绝
        let err = s.forget_episode("ep-1", None, 1).unwrap_err();
        assert!(matches!(err, MemoryGovernanceError::Protected(_)));
        // unprotect 后可 forget
        s.unprotect_episode("ep-1", 1).unwrap();
        let g = s.forget_episode("ep-1", None, 2).unwrap();
        assert_eq!(g.status, MemoryGovernanceStatus::Forgotten);
    }

    #[test]
    fn memory_concurrent_revision_conflict() {
        let s = store();
        put(&s, "ep-1", "me", "x");
        s.update_episode_content("ep-1", "A 改", None, 0).unwrap(); // rev 0→1
        let err = s
            .update_episode_content("ep-1", "B 改", None, 0)
            .unwrap_err(); // stale rev
        match err {
            MemoryGovernanceError::Conflict {
                expected, actual, ..
            } => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn memory_restart_persistence() {
        let s = store();
        put(&s, "ep-1", "me", "x");
        s.protect_episode("ep-1", 0).unwrap();
        s.update_episode_content("ep-1", "改名", None, 1).unwrap();
        let g = s.get_governed("ep-1").unwrap().unwrap();
        assert!(g.protected);
        assert_eq!(g.episode.content, "改名");
        assert_eq!(g.revision, 2);
    }

    #[test]
    fn memory_graph_integrity_factg_forgotten_filtered() {
        // factg-* / link-* 存为 episodes. forget 一个 factg → governed 检索排除,
        // 不留 dangling (link 指向的 episode 仍存在).
        let s = store();
        put(&s, "factg-1", "me", r#"{"s":"a","p":"likes","o":"b"}"#);
        put(&s, "link-1", "me", r#"{"from":"factg-1","to":"ep-1"}"#);
        s.forget_episode("factg-1", None, 0).unwrap();
        let q = EpisodeQuery::new().for_session("me");
        let gov = s.governed_query(&q).unwrap();
        // factg-1 forgotten → 排除; link-1 仍 active
        assert_eq!(gov.len(), 1);
        assert_eq!(gov[0].episode.id, "link-1");
    }

    #[test]
    fn memory_legacy_episode_no_governance_default_active() {
        // 存量 episode 无 governance 行 → LEFT JOIN NULL → 默认 active/unprotected/rev0.
        let s = store();
        put(&s, "ep-1", "me", "x");
        let g = s.get_governed("ep-1").unwrap().unwrap();
        assert_eq!(g.status, MemoryGovernanceStatus::Active);
        assert!(!g.protected);
        assert_eq!(g.revision, 0);
        // 出现在 governed 检索
        let recent = s.governed_recent_episodes("me", 10).unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn memory_not_found_errors() {
        let s = store();
        let err = s.update_episode_content("ghost", "x", None, 0).unwrap_err();
        assert!(matches!(err, MemoryGovernanceError::NotFound(_)));
        let err = s.forget_episode("ghost", None, 0).unwrap_err();
        assert!(matches!(err, MemoryGovernanceError::NotFound(_)));
        assert!(s.get_governed("ghost").unwrap().is_none());
    }
}
