//! Schema migrations for `apeireth-memory`.
//!
//! 按主人偏好: 直接 SQL, 不引入 ORM. 每次新 schema 改动都追加一条新 entry
//! 到 [`MIGRATIONS`], 不可修改历史 (migrations are append-only too).
//!
//! 命名规范: `V<version>__<short_description>`.

use rusqlite::{params, Connection, Transaction};

use crate::MemoryError;
use crate::MemoryResult;

/// 单条 schema migration.
pub struct Migration {
    /// 顺序版本号 (单调递增).
    pub version: i64,
    /// 人类可读名称.
    pub name: &'static str,
    /// 该 migration 实际执行的 SQL (可在事务中批量执行).
    pub sql: &'static str,
}

/// 全部已实装 migrations.
///
/// ⚠️ Append-only: 不要修改既有 entry, 只能追加新 entry.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "V1__init_six_history_streams",
        sql: INIT_SQL,
    },
    // R179 P1-10: Hallway 表 (wing 内 entity-pair co-occurrence)
    // 跟 6 历史流不一样: 不加 append-only trigger, 允许 recompute 时 UPSERT
    // (mempalace "preserve L7 dynamics" — strength/stability 必须跨 recompute 保留).
    Migration {
        version: 2,
        name: "V2__hallways_for_wings",
        sql: HALLWAYS_SQL,
    },
    // M5 (记忆调研批⭐): notes 时间有效性 valid_from/valid_until.
    // 向后兼容铁律: 两列均 NULLable, 存量行 ALTER 后自动 NULL = 永久有效, 零数据迁移.
    // SQLite ALTER TABLE 一次只能加一列 → 两条语句. 不加索引: 过滤条件含
    // "IS NULL OR" 析取本身非 sargable, notes 表规模下全扫足够 (升级路径: 表大后
    // 可加 valid_until 部分索引, 见 docs/backlog.md M5 记录).
    Migration {
        version: 3,
        name: "V3__notes_validity_window",
        sql: "ALTER TABLE notes ADD COLUMN valid_from INTEGER;\n\
              ALTER TABLE notes ADD COLUMN valid_until INTEGER;",
    },
    // TP24 (M5 + N25): episodes 表加 4 列元数据 — 来源链 (provenance) + 时间窗 (ms 精度).
    // 加列而非改列 (per task 纪律): 存量行 4 列均为 NULL, 读取时按 normalize_meta 兜底:
    //   provenance NULL → Manual, valid_from NULL → created_ms, valid_until NULL → None (永久),
    //   created_ms NULL → timestamp * 1000 (s → ms 兜).
    // 不加索引: V4 查询条件含 "IS NULL OR" 析取本身非 sargable, episodes 表规模下全扫足够.
    // 升级路径: 表大后可加 created_ms 部分索引 / 表达式索引, 见 docs/backlog.md M5 记录.
    // SQLite ALTER TABLE 一次一列 → 4 条语句.
    Migration {
        version: 4,
        name: "V4__episodes_provenance_and_timing",
        sql: "ALTER TABLE episodes ADD COLUMN valid_from_ms INTEGER;\n\
              ALTER TABLE episodes ADD COLUMN valid_until_ms INTEGER;\n\
              ALTER TABLE episodes ADD COLUMN created_ms INTEGER;\n\
              ALTER TABLE episodes ADD COLUMN provenance TEXT;\n\
              CREATE INDEX IF NOT EXISTS idx_episodes_created_ms ON episodes(created_ms);",
    },
    // Core Capability Expansion Phase 2: sessions 表生命周期扩展列.
    // 向后兼容铁律: 全部新增列 NULLable / 有默认值, 存量行 ALTER 后自动取默认语义
    // (title NULL→"未命名", state NULL→active, revision NULL→0, scope NULL→global).
    // 不改既有 sessions 列定义, 不动既有 SessionStore trait 行为 (旧 upsert 仍只写 4 列).
    // SQLite ALTER TABLE 一次一列 → 多条语句.
    Migration {
        version: 5,
        name: "V5__sessions_lifecycle",
        sql: "ALTER TABLE sessions ADD COLUMN title TEXT;\n\
              ALTER TABLE sessions ADD COLUMN scope TEXT;\n\
              ALTER TABLE sessions ADD COLUMN project_id TEXT;\n\
              ALTER TABLE sessions ADD COLUMN state TEXT;\n\
              ALTER TABLE sessions ADD COLUMN metadata_json TEXT;\n\
              ALTER TABLE sessions ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;\n\
              ALTER TABLE sessions ADD COLUMN archived_at INTEGER;\n\
              ALTER TABLE sessions ADD COLUMN updated_at INTEGER;\n\
              CREATE INDEX IF NOT EXISTS idx_sessions_state ON sessions(state);\n\
              CREATE INDEX IF NOT EXISTS idx_sessions_scope ON sessions(scope);",
    },
    // Core Capability Expansion Phase 3: 记忆治理 — episode governance 表.
    // episodes 表是 append-only (trigger 拒绝 UPDATE/DELETE), 不能直接改其内容/状态.
    // 治理层用独立 sidecar 表记录可变元数据: forgotten (软删, 从检索排除) /
    // protected (防自动遗忘) / content_override (修订内容) / revision (乐观并发).
    // 原始 episode 行不动 → provenance 完整 + 审计可追溯. 存量行无 governance 记录 =
    // 默认 active/unprotected (LEFT JOIN NULL 语义), 零数据迁移.
    Migration {
        version: 6,
        name: "V6__episode_governance",
        sql: "CREATE TABLE IF NOT EXISTS episode_governance (\n\
              episode_id    TEXT PRIMARY KEY,\n\
              status        TEXT NOT NULL DEFAULT 'active',\n\
              protected     INTEGER NOT NULL DEFAULT 0,\n\
              content_override TEXT,\n\
              revision      INTEGER NOT NULL DEFAULT 0,\n\
              updated_at    INTEGER,\n\
              updated_by    TEXT,\n\
              reason        TEXT,\n\
              forgotten_at  INTEGER\n\
              );\n\
              CREATE INDEX IF NOT EXISTS idx_episode_governance_status ON episode_governance(status);\n\
              CREATE INDEX IF NOT EXISTS idx_episode_governance_protected ON episode_governance(protected);",
    },
    // Core Capability Expansion Phase 5: Agent 执行轨迹 (structured trace).
    // 一次用户请求 → trace_id; Commander/Worker/Tool/Memory 各为 span (parent_span_id 关联).
    // append-only (每 span 一行, 终态时写 ended_at/status). 属性 attributes_json 已 redaction.
    // **严禁**存储模型内部原始 Chain-of-Thought; 只存 safe user-facing summary.
    // trace_id/span_id 用 16-hex (与 telemetry W3C span 同形态, 便于未来打通).
    Migration {
        version: 7,
        name: "V7__agent_traces",
        sql: "CREATE TABLE IF NOT EXISTS agent_traces (\n\
              span_id        TEXT PRIMARY KEY,\n\
              trace_id       TEXT NOT NULL,\n\
              parent_span_id TEXT,\n\
              kind           TEXT NOT NULL,\n\
              actor          TEXT NOT NULL,\n\
              status         TEXT NOT NULL DEFAULT 'running',\n\
              summary        TEXT,\n\
              attributes_json TEXT NOT NULL DEFAULT '{}',\n\
              started_at     INTEGER NOT NULL,\n\
              ended_at       INTEGER,\n\
              session_id     TEXT\n\
              );\n\
              CREATE INDEX IF NOT EXISTS idx_agent_traces_trace ON agent_traces(trace_id, started_at);\n\
              CREATE INDEX IF NOT EXISTS idx_agent_traces_session ON agent_traces(session_id, started_at);\n\
              CREATE INDEX IF NOT EXISTS idx_agent_traces_started ON agent_traces(started_at DESC);",
    },
    // B2 · Phase 1 (research, 默认关闭): 派生记忆血缘 + append-only 血缘审计事件.
    // - research_derived_from: RA-1 A.4.1 schema (主键 = 全列去重, 幂等登记).
    // - research_lineage_events: A8 不可变审计事件 (只 INSERT, 无 UPDATE/DELETE 路径).
    // 纯新增表: 不影响任何既有表/查询语义; 生产路径零触碰.
    Migration {
        version: 8,
        name: "V8__research_derived_lineage",
        sql: "CREATE TABLE IF NOT EXISTS research_derived_from (\n\
              derived_kind TEXT NOT NULL,\n\
              derived_id   TEXT NOT NULL,\n\
              source_kind  TEXT NOT NULL,\n\
              source_id    TEXT NOT NULL,\n\
              span         TEXT,\n\
              ts           INTEGER NOT NULL,\n\
              PRIMARY KEY (derived_kind, derived_id, source_kind, source_id)\n\
              );\n\
              CREATE INDEX IF NOT EXISTS idx_research_derived_src ON research_derived_from(source_kind, source_id);\n\
              CREATE INDEX IF NOT EXISTS idx_research_derived_der ON research_derived_from(derived_kind, derived_id);\n\
              CREATE TABLE IF NOT EXISTS research_lineage_events (\n\
              seq              INTEGER PRIMARY KEY AUTOINCREMENT,\n\
              op               TEXT NOT NULL,\n\
              actor            TEXT,\n\
              ts               INTEGER NOT NULL,\n\
              reason           TEXT,\n\
              subject          TEXT NOT NULL,\n\
              revision_before  INTEGER,\n\
              revision_after   INTEGER,\n\
              detail_json      TEXT NOT NULL DEFAULT '{}'\n\
              );\n\
              CREATE INDEX IF NOT EXISTS idx_research_lineage_events_ts ON research_lineage_events(ts);",
    },
    // Memory 2.1: additive scope/provenance metadata sidecar. Episodes stay
    // append-only; existing rows without metadata are interpreted as
    // session-scoped legacy records by the coordinator.
    Migration {
        version: 9,
        name: "V9__episode_memory_scope_metadata",
        sql: r#"
CREATE TABLE IF NOT EXISTS episode_memory_metadata (
    episode_id TEXT PRIMARY KEY,
    metadata_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_episode_memory_metadata_scope
    ON episode_memory_metadata(json_extract(metadata_json, '$.scope.scope'));
"#,
    },
];

const HALLWAYS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS hallways (
    id                 TEXT PRIMARY KEY,
    wing               TEXT NOT NULL,
    entity_a           TEXT NOT NULL,
    entity_b           TEXT NOT NULL,
    co_occurrence_count INTEGER NOT NULL,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    -- L7 dynamics (mempalace preserve-on-recompute)
    strength           REAL NOT NULL DEFAULT 1.0,
    stability          REAL NOT NULL DEFAULT 1.0,
    last_activated     INTEGER,
    access_count       INTEGER NOT NULL DEFAULT 0,
    tombstoned_at      INTEGER
);

CREATE INDEX IF NOT EXISTS idx_hallways_wing ON hallways(wing);
CREATE INDEX IF NOT EXISTS idx_hallways_pair ON hallways(entity_a, entity_b);
CREATE INDEX IF NOT EXISTS idx_hallways_entity_a ON hallways(entity_a);
CREATE INDEX IF NOT EXISTS idx_hallways_entity_b ON hallways(entity_b);
"#;

const INIT_SQL: &str = r#"
-- === A4 主目标: 6 历史流 Append-only Log ===
-- 表命名严格按 StreamKind::table_name() 一一对应, 6 张独立表 (非 1 张混合表).

CREATE TABLE IF NOT EXISTS thought_stream (
    id           TEXT PRIMARY KEY,
    subject_id   TEXT NOT NULL,
    subject_rev  INTEGER NOT NULL,
    session_id   TEXT,
    created_at   INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',
    tombstoned_at INTEGER
);

CREATE TABLE IF NOT EXISTS proposal_stream (
    id           TEXT PRIMARY KEY,
    subject_id   TEXT NOT NULL,
    subject_rev  INTEGER NOT NULL,
    session_id   TEXT,
    created_at   INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',
    tombstoned_at INTEGER
);

CREATE TABLE IF NOT EXISTS action_stream (
    id           TEXT PRIMARY KEY,
    subject_id   TEXT NOT NULL,
    subject_rev  INTEGER NOT NULL,
    session_id   TEXT,
    created_at   INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',
    tombstoned_at INTEGER
);

CREATE TABLE IF NOT EXISTS relation_stream (
    id           TEXT PRIMARY KEY,
    subject_id   TEXT NOT NULL,
    subject_rev  INTEGER NOT NULL,
    session_id   TEXT,
    created_at   INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',
    tombstoned_at INTEGER
);

CREATE TABLE IF NOT EXISTS evolution_stream (
    id           TEXT PRIMARY KEY,
    subject_id   TEXT NOT NULL,
    subject_rev  INTEGER NOT NULL,
    session_id   TEXT,
    created_at   INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',
    tombstoned_at INTEGER
);

CREATE TABLE IF NOT EXISTS reflection_stream (
    id           TEXT PRIMARY KEY,
    subject_id   TEXT NOT NULL,
    subject_rev  INTEGER NOT NULL,
    session_id   TEXT,
    created_at   INTEGER NOT NULL,
    payload      TEXT NOT NULL,
    source       TEXT NOT NULL,
    tags         TEXT NOT NULL DEFAULT '[]',
    tombstoned_at INTEGER
);

-- 6 张表共享的索引: (subject_id, created_at) 便于按主体按时间窗查询.
CREATE INDEX IF NOT EXISTS idx_thought_subject      ON thought_stream(subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_proposal_subject     ON proposal_stream(subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_action_subject       ON action_stream(subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_relation_subject     ON relation_stream(subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_evolution_subject    ON evolution_stream(subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_reflection_subject   ON reflection_stream(subject_id, created_at);

-- === IdentityCard 跨载体唯一 (D2 §4 主体连续性) ===
-- 物理上允许 UPDATE (用于: 跨载体迁移追加 migration_history、软删除设置 tombstoned_at)
-- 物理上禁止硬 DELETE (主体连续性记录不可抹除, 只能 tombstone)
CREATE TABLE IF NOT EXISTS identity_cards (
    continuity_id    TEXT PRIMARY KEY,        -- 跨载体唯一
    birth_time       INTEGER NOT NULL,
    carriers_json    TEXT NOT NULL DEFAULT '[]',
    migration_history_json TEXT NOT NULL DEFAULT '[]',
    subject_rev      INTEGER NOT NULL DEFAULT 0,  -- D2 §4.3 主体版本号
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    tombstoned_at    INTEGER,                       -- 软删除标记
    tombstoned_reason TEXT
);
CREATE INDEX IF NOT EXISTS idx_identity_cards_birth  ON identity_cards(birth_time);
CREATE INDEX IF NOT EXISTS idx_identity_cards_tomb   ON identity_cards(tombstoned_at);
CREATE TRIGGER IF NOT EXISTS identity_cards_no_delete
BEFORE DELETE ON identity_cards
BEGIN
    SELECT RAISE(ABORT, 'identity_cards: hard DELETE forbidden, use tombstone() instead');
END;

-- === Episode 表 (按 session_id / time range / continuity_id 索引查询) ===
CREATE TABLE IF NOT EXISTS episodes (
    id           TEXT PRIMARY KEY,
    continuity_id TEXT NOT NULL,               -- 跨载体主体引用
    session_id   TEXT NOT NULL,
    timestamp    INTEGER NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_episodes_session   ON episodes(session_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_episodes_subject   ON episodes(continuity_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_episodes_time      ON episodes(timestamp);

-- Episode 同样 append-only: 写入即不可变.
CREATE TRIGGER IF NOT EXISTS episodes_no_update
BEFORE UPDATE ON episodes
BEGIN
    SELECT RAISE(ABORT, 'episodes is append-only: cannot mutate historical events');
END;
CREATE TRIGGER IF NOT EXISTS episodes_no_delete
BEFORE DELETE ON episodes
BEGIN
    SELECT RAISE(ABORT, 'episodes is append-only: tombstone via reflection_stream instead');
END;

-- === Session + Note (A1 兼容 + Note 可更新) ===
CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,
    started_at    INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL,
    closed_at     INTEGER
);

CREATE TABLE IF NOT EXISTS notes (
    id                 TEXT PRIMARY KEY,
    timestamp          INTEGER NOT NULL,
    content            TEXT NOT NULL,
    source_episode_ids_json TEXT NOT NULL DEFAULT '[]',
    confidence         REAL NOT NULL DEFAULT 0.5,
    tags_json          TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_notes_time ON notes(timestamp);
CREATE INDEX IF NOT EXISTS idx_notes_conf ON notes(confidence);
-- Note 可被遗忘/合并 (apeireth_core::kernel::memory::Note 文档明确), 不强制 append-only.

-- === Migration tracking ===
CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    applied_at  INTEGER NOT NULL
);
"#;

const APPEND_ONLY_TRIGGERS: &[(&str, &str)] = &[
    (
        "thought_stream",
        "thought_stream is append-only: D2 §5.3 #1 prohibits in-place mutation",
    ),
    (
        "proposal_stream",
        "proposal_stream is append-only: D2 §5.3 #1 prohibits in-place mutation",
    ),
    (
        "action_stream",
        "action_stream is append-only: D2 §5.3 #1 prohibits in-place mutation",
    ),
    (
        "relation_stream",
        "relation_stream is append-only: D2 §5.3 #1 prohibits in-place mutation",
    ),
    (
        "evolution_stream",
        "evolution_stream is append-only: D2 §5.3 #1 prohibits in-place mutation",
    ),
    (
        "reflection_stream",
        "reflection_stream is append-only: D2 §5.3 #1 prohibits in-place mutation",
    ),
];

/// 应用全部未执行的 migrations.
///
/// 事务保护: 单条 migration 内所有 DDL 在一个事务内执行, 失败回滚.
pub fn run_migrations(conn: &mut Connection) -> MemoryResult<()> {
    // 1. 建表总是先跑一次 (CREATE IF NOT EXISTS) — 后续 migration 再叠加 trigger.
    let tx = conn.transaction()?;
    tx.execute_batch(INIT_SQL)?;
    tx.commit()?;

    // 2. 注册 6 流 append-only triggers (幂等).
    // 规则: 禁止任何原地修改; 唯一例外是 "软删除" (tombstoned_at 从 NULL 变为非 NULL).
    // 软删除也是不可逆的 (OLD.tombstoned_at 必须是 NULL).
    for (table, reason) in APPEND_ONLY_TRIGGERS {
        let update_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS {table}_no_inplace_update
             BEFORE UPDATE ON {table}
             FOR EACH ROW
             WHEN NOT (NEW.tombstoned_at IS NOT NULL AND OLD.tombstoned_at IS NULL)
             BEGIN
                 SELECT RAISE(ABORT, '{reason}');
             END;"
        );
        let delete_trigger = format!(
            "CREATE TRIGGER IF NOT EXISTS {table}_no_delete
             BEFORE DELETE ON {table}
             BEGIN
                 SELECT RAISE(ABORT, '{reason}');
             END;"
        );
        conn.execute_batch(&update_trigger)?;
        conn.execute_batch(&delete_trigger)?;
    }

    // 3. 记录已应用的 migration.
    for m in MIGRATIONS {
        if migration_applied(conn, m.version)? {
            continue;
        }
        let tx: Transaction<'_> = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            params![m.version, m.name, now_unix()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn migration_applied(conn: &Connection, version: i64) -> MemoryResult<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
            params![version],
            |row| row.get(0),
        )
        .map_err(MemoryError::from)?;
    Ok(count > 0)
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteMemoryStore;

    #[test]
    fn migrations_apply_idempotently() {
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        let first = store.applied_migrations().unwrap();
        assert!(first.contains(&1), "expected V1 applied");
        // 二次 open: 不应重复插入.
        let conn = store.conn().expect("store conn");
        let mut guard = conn;
        run_migrations(&mut guard).unwrap();
        let second = SqliteMemoryStore::open_in_memory()
            .unwrap()
            .applied_migrations()
            .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn append_only_triggers_reject_inplace_update_and_delete() {
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        let conn = store.conn().expect("store conn");
        // 直接 raw SQL 验证: 写入一行后, UPDATE 必失败.
        conn.execute(
            "INSERT INTO thought_stream (id, subject_id, subject_rev, created_at, payload, source)
             VALUES ('t1', 'subj', 1, 0, 'p', 'unit')",
            [],
        )
        .unwrap();
        // 改 payload 应被 trigger 拒绝
        let update_err = conn
            .execute(
                "UPDATE thought_stream SET payload = 'x' WHERE id = 't1'",
                [],
            )
            .unwrap_err();
        assert!(
            update_err.to_string().contains("append-only"),
            "expected append-only error, got {update_err}"
        );
        // 改 subject_id 也应被拒绝
        let subj_err = conn
            .execute(
                "UPDATE thought_stream SET subject_id = 'new' WHERE id = 't1'",
                [],
            )
            .unwrap_err();
        assert!(
            subj_err.to_string().contains("append-only"),
            "got {subj_err}"
        );
        // 软删除 (tombstoned_at 从 NULL → 非 NULL) 应被允许
        conn.execute(
            "UPDATE thought_stream SET tombstoned_at = 1000 WHERE id = 't1'",
            [],
        )
        .expect("soft delete should be allowed");
        // 二次软删除 (tombstoned_at 已非 NULL) 应被拒绝
        let re_tomb = conn
            .execute(
                "UPDATE thought_stream SET tombstoned_at = 2000 WHERE id = 't1'",
                [],
            )
            .unwrap_err();
        assert!(re_tomb.to_string().contains("append-only"), "got {re_tomb}");
        // 物理 DELETE 仍被拒绝
        let delete_err = conn
            .execute("DELETE FROM thought_stream WHERE id = 't1'", [])
            .unwrap_err();
        assert!(delete_err.to_string().contains("append-only"));
    }

    // ===== Core Capability Expansion Phase 7: migration integrity =====

    #[test]
    fn all_migrations_v1_to_v7_applied_on_fresh_db() {
        // 全新 in-memory DB: V1-V7 全部应用, schema_migrations 记录完整.
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        let applied = store.applied_migrations().unwrap();
        for v in 1..=9 {
            assert!(
                applied.contains(&v),
                "migration V{v} should be applied on fresh db"
            );
        }
        // V5/V6/V7 新表/列存在.
        let conn = store.conn().unwrap();
        // sessions 生命周期列 (V5).
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(sessions)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for c in [
            "title",
            "scope",
            "project_id",
            "state",
            "metadata_json",
            "revision",
            "archived_at",
            "updated_at",
        ] {
            assert!(
                cols.iter().any(|x| x == c),
                "sessions should have column {c}"
            );
        }
        // episode_governance 表 (V6).
        let gov_exists: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='episode_governance')", [], |r| r.get(0))
            .unwrap();
        assert!(gov_exists, "episode_governance table should exist");
        // agent_traces 表 (V7).
        let trace_exists: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_traces')", [], |r| r.get(0))
            .unwrap();
        assert!(trace_exists, "agent_traces table should exist");
    }

    #[test]
    fn migrations_reopen_preserves_data_and_idempotent() {
        // 写入跨 V5/V6/V7 的数据, 重跑 migration, 数据不丢 + 幂等.
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        use crate::agent_trace::{TraceSpan, TraceSpanKind, TraceSpanStatus};
        use crate::episode::EpisodeStore;
        use crate::{MemoryGovernanceStore, SessionLifecycleStore, TraceStore};
        use apeireth_core::kernel::memory::Episode;
        // session lifecycle (V5).
        store
            .create_session(
                "s1",
                Some("持久"),
                crate::session_lifecycle::SessionScope::Global,
                None,
                None,
            )
            .unwrap();
        // episode + governance (V6).
        store
            .put_episode(&Episode {
                id: "ep-1".into(),
                timestamp: 1,
                role: "user".into(),
                content: "x".into(),
                session_id: "s1".into(),
            })
            .unwrap();
        store.protect_episode("ep-1", 0).unwrap();
        // trace (V7).
        let span = TraceSpan {
            span_id: "sp1".into(),
            trace_id: "t1".into(),
            parent_span_id: None,
            kind: TraceSpanKind::Conversation,
            actor: "user".into(),
            status: TraceSpanStatus::Succeeded,
            summary: Some("done".into()),
            attributes: serde_json::json!({}),
            started_at: 1,
            ended_at: Some(2),
            session_id: Some("s1".into()),
        };
        store.put_trace_span(&span).unwrap();
        // 重跑 migration (幂等).
        {
            let guard = store.conn().unwrap();
            let mut g = guard;
            run_migrations(&mut g).unwrap();
        }
        // 数据仍在.
        let s = store.get_session_lifecycle("s1").unwrap().unwrap();
        assert_eq!(s.title.as_deref(), Some("持久"));
        let g = store.get_governed("ep-1").unwrap().unwrap();
        assert!(g.protected);
        let t = store.get_trace_span("sp1").unwrap().unwrap();
        assert_eq!(t.trace_id, "t1");
        // migration 记录无重复.
        let applied = store.applied_migrations().unwrap();
        assert_eq!(
            applied.iter().filter(|v| **v >= 5).count(),
            5,
            "V5/V6/V7/V8/V9 各一条"
        );
    }

    #[test]
    fn legacy_episode_works_after_v6_governance_added() {
        // 旧 episode (无 governance 行) 在 V6 后仍可正常检索 + 默认 active.
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        use crate::episode::EpisodeStore;
        use crate::MemoryGovernanceStore;
        store
            .put_episode(&apeireth_core::kernel::memory::Episode {
                id: "legacy-ep".into(),
                timestamp: 1,
                role: "user".into(),
                content: "old".into(),
                session_id: "me".into(),
            })
            .unwrap();
        let recent = store.governed_recent_episodes("me", 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(
            recent[0].status,
            crate::memory_governance::MemoryGovernanceStatus::Active
        );
        assert!(!recent[0].protected);
    }
}
