//! apeireth-memory: 记忆子系统 (Episode/Note/Session SQLite 存储 + 6 历史流 Append-only Log + IdentityCard 跨载体唯一)
//!
//! R14 A4 成就落地:
//! 1. SQLite schema = 6 历史流表 (思想/提案/行动/关系/演化/反思期)
//!                  + `identity_cards` (continuity_id UNIQUE 跨载体)
//!                  + `episodes` (按 session_id / time range / continuity_id 索引查询)
//! 2. 6 个 Append-only Log trait: 思想/提案/行动/关系/演化/反思期
//! 3. Append-only = `BEFORE UPDATE` / `BEFORE DELETE` triggers raise ABORT
//! 4. IdentityCard.continuity_id = UNIQUE 约束, 跨载体去重
//! 5. Episode 写入 + 查询 API (按 session_id / time range / continuity_id)
//! 6. 直接 SQL (主人偏好: 不引入 ORM)
//!
//! 禁止:
//! - ❌ 不修改 apeireth-core 任何已实装类型签名
//! - ❌ 不引入 ORM (按主人偏好)
//! - ❌ 不碰 R11 baseline 三值
//! - ❌ 不碰 apeireth-legacy/

#![deny(unsafe_code)]

use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard};

use apeireth_core::{
    kernel::memory::Episode, kernel::memory::IdentityCard, kernel::memory::Note,
    kernel::memory::Session,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod append_only;
pub mod canonical;
mod episode;
mod identity;
mod migrations;
// N8: generation 绑定观测缓存 (自包含, VCP MemoRuntime 精神, artifact_sig 联动口; 移交续接; merge 吞行后二次补回)
pub mod gen_cache;
// R179 P1-10: Hallway — wing 内 entity-pair 跨位置走廊 (借鉴 mempalace hallways.py)
pub mod hallways;
mod session_note;
mod streams;
mod three_layer; // R30 U9: claude-mem 3 层 facade

pub use append_only::{AppendOnlyError, HistoryEntry, HistoryStream, Tombstone};
// R22 ST-A2.4 — 6 历史流深度公共 API (query / insert / count)
pub mod history_streams;

pub mod arbitration;
pub mod betti_hole_detector;
pub mod bitemporal_graph;
pub mod chronicle_crystallizer;
pub mod continuity_link;
pub use continuity_link::{
    continuity_id_from_env, current_continuity_id, ensure_identity, migrate_subject,
    normalize_continuity, recall_recent, record_carrier_migration, record_session,
    resolve_continuity, ContinuityLink, MigrationReport, SessionRef, CONTINUITY_ENV_VAR,
    DEFAULT_CONTINUITY_ID, MIGRATED_ID_PREFIX,
};
pub mod calibration;
pub mod calibration_critic;
pub mod cluster_store;
pub mod confidence;
pub mod cross_diary;
pub mod daily_summary;
pub mod diary;
pub mod dreaming;
pub mod ensemble;
pub mod five_dimensional;
pub mod graph_algo;
pub mod hybrid_search;
pub mod intent_brier;
pub mod kuramoto_resonance;
pub mod meta_thinking;
pub mod metadata_filter;
pub mod milestone;
pub mod online_calibration;
pub mod partner;
pub mod persistent_vector;
pub mod principles;
pub mod procedural;
pub mod query_expand;
pub mod reflexion;
pub mod residual_pyramid;
pub mod river_topology;
pub mod semantic_axis;
pub mod three_tier_vault;
pub mod topic_predictor;
pub mod vector_distance;
pub mod wiki_fs;
// Salvage-03 (memory-advanced): closed-world injection + A-MEM residual CRAWL.
// Default-off; not production-wired.
pub mod amem_graph;
pub mod dream_consolidation;
pub mod memory_injection;
pub mod memory_rank;

pub use betti_hole_detector::{
    BettiHoleDetector, BettiTopologicalReport, ManifoldConceptNode, TopologicalVoidRing,
};
pub use chronicle_crystallizer::{ChronicleCrystallizer, ChronicleSection, RawEpisodicTrace};
pub use kuramoto_resonance::{EpiphanyEvent, KuramotoOscillator, KuramotoResonanceEngine};

pub use amem_graph::{
    combined_score, content_residual, crawl, fact_specificity, text_overlap, AmemGraph, GraphFact,
    GraphRankConfig, MemoryLink, GRAPH_INJECTION_LIMIT, LINK_OVERLAP_THRESHOLD,
};
pub use dream_consolidation::{pair_merge, select_dream_candidates, DreamSource, DREAM_ID_PREFIX};
pub use memory_injection::{
    build_memory_injection, build_preference_injection, EVIDENCE_MAX_CHARS,
    PREFERENCE_INJECTION_LIMIT,
};
pub use memory_rank::{
    filter_active_memories, group_bonus, memory_score, parse_importance, rank_memory_entries,
    recency_score, RankableMemory, IMP_PREFIX, TOMBSTONE_PREFIX,
};

pub use residual_pyramid::{
    FieldActivationGate, OrthogonalResidualPyramid, PyramidAnalysis, PyramidLevel,
};
pub use river_topology::{
    DtscObservables, DualScaledFieldSolver, RiverDynamicsEngine, RiverEdge, RiverObservability,
    RiverState, SpikeSignal, TagNode,
};
pub use semantic_axis::{SemanticAxisBridge, SemanticAxisProjection};
pub use three_tier_vault::{
    ProvenanceRecord, ThreeTierVault, TocTreeIndexer, TocTreeNode, TreeReasoningRouter, VaultError,
    VaultTier,
};

pub use bitemporal_graph::{BitemporalFact, BitemporalGraph};
pub use five_dimensional::{
    FactItem, FiveDimensionalMemory, MemoryBrowserEntry, MemoryDimension, ReflectionItem,
};
pub use wiki_fs::{WikiFsEngine, WikiHealthReport, WikiLintIssue, WikiPage};

pub use arbitration::{
    ArbitrationEngine, ArbitrationError, ArbitrationEvent, EventSource, IntegrityReport,
};

pub use dreaming::{DreamEngine, DreamEngineConfig, DreamError, DreamReport, DreamStage};

pub use procedural::{
    render_procedural_prompt, HabitMatch, HabitPattern, InMemoryProceduralStore, ProceduralError,
    ProceduralStore,
};

pub use meta_thinking::{
    save_to_cluster, ChainReflectionThinker, ChainStage, MetaChainResult, MetaThinkError,
    MetaThinkInput, MetaThinkOutput, MetaThinker, MetaThinkingChain, ReflectionMetaThinker,
    StageResult, StopReason, DEFAULT_MAX_DEPTH,
};

pub use cluster_store::{
    ClusterFile, ClusterReader, ClusterStore, ClusterStoreError, InMemoryClusterReader,
    CLUSTER_SUFFIX, META_CHAINS_FILE, MIN_EDIT_TARGET_CHARS,
};

pub use intent_brier::{
    brier_score, compute_report, compute_trend, compute_window, domain_diagnostics, mean_brier,
    render_report, BrierTrend, BrierWindow, DomainDiagnostic, FeedbackOutcome,
    IntentDiagnosticReport, IntentLedger, IntentPrediction, IntentRecord,
    DEFAULT_LOW_CALIBRATION_THRESHOLD, DEFAULT_WINDOWS, TREND_DELTA_RATIO,
};

pub use calibration::{
    brier_squared, calibration_bins, decompose, decompose_default, ece_default,
    expected_calibration_error, mean_brier_score, BrierDecomposition, CalibrationBin, Observation,
    DEFAULT_NUM_BINS,
};
pub use calibration_critic::{
    CalibrationCritic, CriticConfig as CalibrationCriticConfig, CritiqueAction, CritiqueResult,
};
pub use confidence::{BetaBinomial, Strength as ConfidenceStrength};
pub use ensemble::{
    AggregationStrategy, EnsembleConfig, EnsembleForecast, EnsembleMember, MarketConfig,
    MarketError, PredictionMarket, TradeReceipt,
};
pub use online_calibration::{
    AdaptiveBaseline, CalibrationCoefficients, Coeff, DriftAlarm, DriftDetector, LinearCalibration,
    RecalibrationScheduler, ScheduleReport, UserFeedback as CalibrationFeedback,
};

pub use reflexion::{
    Critic, FailureKind, FailureRecord, FileReflexionStore, InMemoryReflexionStore, ReflectionText,
    ReflexionError, ReflexionStore, RuleCritic,
};

pub use cross_diary::{link_core, CrossDiaryIndex, CrossLink, SNIPPET_MAX_CHARS};
pub use daily_summary::{build_daily_summary, DailySummary};
pub use diary::{
    valid_date, DayPage, DiaryEntry, DiaryError, DiaryHit, DiaryInjector, DiaryStore,
    FileDiaryStore, InMemoryDiaryStore, TRUNCATION_MARK,
};

pub use episode::{EpisodeQuery, EpisodeStore};
pub use graph_algo::{
    all_paths, connected_components, dijkstra_shortest_path, edges_matching, has_cycle,
    neighbors_directed, nodes_with_label, topological_sort, walk, TraversalDirection, WalkOrder,
    WalkStep,
};
pub use hybrid_search::{tokenize, Bm25Config, Bm25Hit, Bm25Index, HybridHit, HybridSearchEngine};
pub use metadata_filter::{MetadataFilter, PropertyPredicate};
pub use milestone::{
    InMemoryMilestoneStore, Milestone, MilestoneKind, MilestonePayload, MilestoneStore,
};
pub use partner::{
    Bond, BondCharacter, BondDepth, BondStage, InMemoryPartnerStore, Partner, PartnerId,
    PartnerPreferences, PartnerStore, PrivacyBoundary,
};
pub use persistent_vector::{PersistentVectorHit, PersistentVectorIndex, DEFAULT_DB_FILE};
pub use principles::{
    check_dynamic_principles, constant_time_eq, DynamicPrinciple, InMemoryPrincipleStore,
    PrincipleStatus, PrincipleStore, PromotionCandidate,
};
pub use query_expand::{expand_query, ExpandedQuery};
pub use topic_predictor::{
    CompositeChannel, ImportanceChannel, KeywordChannel, PreloadChannel, TimeChannel, TopicCue,
    TopicHint, TopicPrediction, TopicPredictor,
};
pub use vector_distance::{
    cosine, cosine_distance, cosine_distance_to_score, distance, dot_product, euclidean_distance,
    euclidean_distance_sq, l2_distance_to_score, l2_norm, manhattan_distance, normalize,
    DistanceMetric,
};
// TP24 (M5 + N25): 记忆来源链 + 时间元数据 (episodes 表的 4 列 V4 扩展).
// 方法以 inherent impl on SqliteMemoryStore 暴露, 不引入 trait (减少 import, 保持向后兼容).
pub mod provenance;
pub use identity::{IdentityCardRecord, IdentityCardStore, IdentityConflict};
pub use migrations::{run_migrations, Migration as SchemaMigration, MIGRATIONS};
pub use provenance::{normalize_meta, validate_meta, EpisodeMeta, Provenance};
pub use session_note::{NoteQuery, NoteRecord, NoteStore, SessionRecord, SessionStore};
// Core Capability Expansion Phase 2: 后端会话生命周期 (state machine + 乐观并发).
// 独立于 SessionStore trait (旧 upsert 不变), 走 inherent impl on SqliteMemoryStore.
pub mod session_lifecycle;
pub use session_lifecycle::{
    SessionLifecycleError, SessionLifecycleRecord, SessionScope, SessionState,
    SessionStore as SessionLifecycleStore,
};
// Core Capability Expansion Phase 3: 记忆治理 (forget/protect/update, 不破坏 append-only episodes).
pub mod memory_governance;
pub use memory_governance::{
    GovernedEpisode, MemoryGovernanceError, MemoryGovernanceStatus, MemoryGovernanceStore,
};
// Core Capability Expansion Phase 5: Agent 执行轨迹 (structured trace, 持久化 + 查询).
pub mod agent_trace;
pub use agent_trace::{
    redact_attributes, sanitize_summary, summary_is_safe, TraceQueryError, TraceSpan,
    TraceSpanKind, TraceSpanStatus, TraceStore,
};
// B2 · Phase 1 (research, 默认关闭): 派生记忆血缘 + 遗忘传播审计 (RA-1 A.4).
pub mod research_derived_memory;
pub use research_derived_memory::{
    dual_rater_protocol, research_invalidate_cache_on_forget, ClosureMode, ClosureNode,
    ClosureReport, DerivedRef, DeterministicLeakJudge, DualRaterResult, GovernedRecall,
    JudgeVerdict, LeakAuditItem, LeakAuditReport, ResearchJudge,
};
// B7 · Phase 6 原型一 (research, 不进默认路径): 漫游记忆 CRDT.
pub mod research_roaming_memory;
pub use research_roaming_memory::{
    ResearchLogicalClock, ResearchRoamingItem, ResearchRoamingMemory,
};
// B8 · Phase 6 原型二 (research, 不进默认路径): 模块非干扰性.
pub mod research_non_interference;
pub use research_non_interference::{
    research_check_non_interference, ResearchCounterModule, ResearchCounterOp, ResearchModule,
    ResearchNonInterferenceReport, ResearchSetModule, ResearchSetOp,
};
// Salvage 02: windowed fingerprint + textual near-dup (companion observer_capture / dream).
pub mod dedup;
pub use dedup::{
    dedup_textual, episode_fingerprint, fingerprint_bytes, fingerprint_json, normalize_for_dedup,
    overlap_ratio, DedupConfig, DedupIndex, DEFAULT_DEDUP_WINDOW_MS, DEFAULT_LRU_CAP,
    TEXTUAL_MIN_LEN, TEXTUAL_OVERLAP_THRESHOLD,
};
// Salvage 02: rolling cross-frontend context ledger.
pub mod context_ledger;
pub use context_ledger::{
    ContextLedger, LedgerEntry, DEFAULT_MAX_RECORDS, ROLE_ASSISTANT, ROLE_USER,
};
// Salvage 02: combined retention sweep (count cap + TTL + decay) via governance sidecar.
pub mod retention;
pub use retention::{decay_strength, sweep_session, RetentionPolicy, RetentionSweepReport};
pub use streams::{
    ActionStream, EvolutionStream, GoalStream, LifeStream, MigrationStream, ProposalStream,
    ReflectionStream, RelationStream, StanceStream, ThoughtStream,
};
pub use three_layer::{ThreeLayerMemory, SHORT_TERM_WINDOW_SECS, WORKING_CAPACITY}; // R30 U9

// Unified Memory 2.0 (coordinator, 4-layer architecture, closed-world prompt injection)
pub mod consolidation;
pub mod context_compiler;
pub mod continuity_state;
pub mod coordinator;
pub mod layers;

pub use consolidation::{ConsolidationReport, MemoryConsolidationJob};
pub use context_compiler::ClosedWorldContextCompiler;
pub use continuity_state::{ContinuityCompressor, ContinuityState};
pub use coordinator::MemoryCoordinator;
pub use layers::{
    MemoryLayerKind, MemoryRecallQuery, MemoryRecallResult, MemoryWritebackEntry,
    RecalledMemoryItem,
};

/// 重新导出 `apeireth_core::kernel::memory::Episode` 方便下游不必记多个导入路径.
pub use apeireth_core::kernel::memory::Episode as CoreEpisode;
// R177: organ invariants (10 tests + 2 Kani proofs)
mod organ_kani_proofs;

/// 顶层错误: 所有 memory 子系统的 fallback error.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// SQLite 底层错误.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Append-only 约束被违反.
    #[error("append-only violation: {0}")]
    AppendOnly(#[from] AppendOnlyError),
    /// IdentityCard continuity_id 冲突.
    #[error("identity conflict: {0}")]
    Identity(#[from] IdentityConflict),
    /// JSON 序列化/反序列化失败.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// P-arch (2026-08-27): 文件后端 / keyring / 未来 storage 路径的 IO 错误.
    /// 同 rusqlite::Error / serde_json::Error 模式：`From` 转换让 `?` 在
    /// FileBackend 等新后端直接工作，无需 `.map_err(...)` 包裹。
    #[error("memory io error: {0}")]
    Io(#[from] std::io::Error),
    /// 调用方提供的参数非法 (空字符串 / 时间范围倒置等).
    #[error("invalid argument: {0}")]
    Invalid(String),
    /// 互斥锁中毒 (panic 持有锁后).
    #[error("memory store mutex poisoned: {0}")]
    Poisoned(String),
    /// Memory subsystem error not covered by a more specific variant.
    #[error("memory subsystem error: {0}")]
    Other(String),
}

/// 统一结果类型.
pub type MemoryResult<T> = Result<T, MemoryError>;

/// 流枚举 (按主人 A4 描述与 D2 §5 三域映射):
/// - Thought      → 思想流 (思想域, 对应 §5 目标史 + 自我叙事)
/// - Proposal     → 提案流 (提案域, 对应 §5 立场史)
/// - Action       → 行动流 (行动域, 对应 §5 生命史)
/// - Relation     → 关系流 (行动域, 对应 §5 关系史)
/// - Evolution    → 演化流 (思想 + 提案, 对应 §5 自我叙事)
/// - Reflection   → 反思期流 (反思期审计, Self-Disable §3 使用)
///
/// **P-arch (2026-08-27) O-6 锚 #2 兑现**: 6 变体 canonical 在 `apeireth_core::kernel::StreamKind`,
/// memory crate 通过 `pub use` re-export 保持 v1 compat. 100+ consumer 0 破 (同类型不同路径).
/// **扩展方法** (`table_name_ext` / `semantic_name_ext`) 由 `apeireth_core::kernel::StreamKindExt`
/// 提供 (在 core kernel 中定义, 避免 orphan rule E0117, 0 改 100+ 调用方).
pub use apeireth_core::kernel::StreamKind;
pub use apeireth_core::kernel::StreamKindExt;

/// (FromStr for memory-local error 删 — StreamKind canonical 在 core kernel,
/// 已有 `impl FromStr for StreamKind` with `Err = String`, orphan rule 阻止 memory 再 impl
/// memory-local `Err = MemoryError`. 提供 `from_str_core` helper 函数把 String 错误 wrap 成 MemoryError.
pub fn from_str_core(s: &str) -> Result<StreamKind, MemoryError> {
    s.parse().map_err(MemoryError::Invalid)
}

/// 统一的内存存储入口 (SQLite 实现).
///
/// 内部持有一个 `Mutex<rusqlite::Connection>`, 默认开启 `WAL` + `foreign_keys`,
/// 并在构造时跑完所有 schema migration (见 [`MIGRATIONS`]).
#[derive(Debug)]
pub struct SqliteMemoryStore {
    conn: Mutex<Connection>,
}

impl SqliteMemoryStore {
    /// 在给定 path 打开一个 SQLite 数据库, 应用 migrations.
    pub fn open(path: impl AsRef<Path>) -> MemoryResult<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.configure()?;
        {
            let mut guard = store.conn.lock().expect("memory store mutex");
            run_migrations(&mut guard)?;
        }
        Ok(store)
    }

    /// 打开一个内存数据库 (主要用于测试, 每次新建独立 store).
    pub fn open_in_memory() -> MemoryResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.configure()?;
        {
            let mut guard = store.conn.lock().expect("memory store mutex");
            run_migrations(&mut guard)?;
        }
        Ok(store)
    }

    fn configure(&self) -> MemoryResult<()> {
        let conn = self.conn.lock().map_err(map_poisoned)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    /// 拿到内部 connection 的锁. 调用方应尽快完成操作并释放.
    pub fn conn(&self) -> MemoryResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(map_poisoned)
    }

    /// 列出已应用的 migration 版本号.
    pub fn applied_migrations(&self) -> MemoryResult<Vec<i64>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT version FROM schema_migrations ORDER BY version ASC")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 一键导出所有 6 历史流条目 (按时间排序), JSON Lines 友好的结构.
    ///
    /// D2 §5.3 硬规则 #4: "可导出" — 6 历史流必须可一键导出.
    pub fn export_streams_jsonl(&self) -> MemoryResult<Vec<HistoryEntry>> {
        let conn = self.conn()?;
        append_only::export_all_streams(&conn)
    }
}

fn map_poisoned(e: std::sync::PoisonError<std::sync::MutexGuard<'_, Connection>>) -> MemoryError {
    MemoryError::Poisoned(e.to_string())
}

// ============================================
// 兼容旧 trait: ContinuitySnapshotStore (A1 阶段 CLI 已引用)
// ============================================

/// ContinuitySnapshotStore trait (Phase 1 实现, 对齐 mvp/memory/store.py).
///
/// 该 trait 在 A1 阶段由 `apeireth-cli` 调用, 不可破坏签名.
/// A4 升级: SqliteMemoryStore 实现了完整版, 含 Append-only Log + IdentityCard.
pub trait ContinuitySnapshotStore: Send {
    /// 写入一个 Episode.
    fn put_episode(&self, ep: &Episode) -> anyhow::Result<()>;
    /// 写入一个 Note.
    fn put_note(&self, note: &Note) -> anyhow::Result<()>;
    /// 检索最近 N 条 Episodes.
    fn recent_episodes(&self, session_id: &str, n: usize) -> anyhow::Result<Vec<Episode>>;
}

impl ContinuitySnapshotStore for SqliteMemoryStore {
    fn put_episode(&self, ep: &Episode) -> anyhow::Result<()> {
        <Self as EpisodeStore>::put_episode(self, ep).map_err(Into::into)
    }

    fn put_note(&self, note: &Note) -> anyhow::Result<()> {
        <Self as NoteStore>::put_note(self, note).map_err(Into::into)
    }

    fn recent_episodes(&self, session_id: &str, n: usize) -> anyhow::Result<Vec<Episode>> {
        <Self as EpisodeStore>::recent_episodes(self, session_id, n).map_err(Into::into)
    }
}

impl apeireth_plugin::memory_backend::MemoryBackend for SqliteMemoryStore {
    fn name(&self) -> &'static str {
        "sqlite_store"
    }

    fn kind(&self) -> apeireth_plugin::memory_backend::BackendKind {
        apeireth_plugin::memory_backend::BackendKind::Sqlite
    }

    fn put_episode(&self, ep: &Episode) -> apeireth_plugin::memory_backend::CapabilityResult<()> {
        <Self as EpisodeStore>::put_episode(self, ep)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    fn get_episode(
        &self,
        id: &str,
    ) -> apeireth_plugin::memory_backend::CapabilityResult<Option<Episode>> {
        <Self as EpisodeStore>::get_episode(self, id)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    fn recent_episodes(
        &self,
        session_id: &str,
        n: usize,
    ) -> apeireth_plugin::memory_backend::CapabilityResult<Vec<Episode>> {
        <Self as EpisodeStore>::recent_episodes(self, session_id, n)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    fn append_stream(
        &self,
        kind: StreamKind,
        entry: HistoryEntry,
    ) -> apeireth_plugin::memory_backend::CapabilityResult<()> {
        let conn = self
            .conn()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        append_only::insert_entry(&conn, kind.table_name_ext(), &entry)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    fn list_stream(
        &self,
        kind: StreamKind,
        _session_id: &str,
        n: usize,
    ) -> apeireth_plugin::memory_backend::CapabilityResult<Vec<HistoryEntry>> {
        let conn = self
            .conn()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        append_only::list_recent_entries(&conn, kind.table_name_ext(), n, false)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }
}

/// 重新导出 `apeireth_core` 给下游 (避免下游写 `apeireth_core::*` 又引一次).
pub use apeireth_core;

#[cfg(test)]
pub(crate) fn fresh_store() -> SqliteMemoryStore {
    SqliteMemoryStore::open_in_memory().expect("open in-memory store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use apeireth_core::kernel::memory::Migration;

    #[test]
    fn open_in_memory_creates_schema() {
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        let migrations = store.applied_migrations().unwrap();
        assert!(
            !migrations.is_empty(),
            "expected at least one migration to be applied"
        );
    }

    #[test]
    fn stream_kind_roundtrip() {
        for kind in StreamKind::ALL {
            let s: &'static str = match kind {
                StreamKind::Thought => "thought",
                StreamKind::Proposal => "proposal",
                StreamKind::Action => "action",
                StreamKind::Relation => "relation",
                StreamKind::Evolution => "evolution",
                StreamKind::Reflection => "reflection",
            };
            assert_eq!(crate::from_str_core(s).unwrap(), kind);
            assert!(!kind.table_name_ext().is_empty());
            assert!(!kind.semantic_name_ext().is_empty());
        }
    }

    #[test]
    fn stream_kind_all_covers_six() {
        assert_eq!(StreamKind::ALL.len(), 6);
        let mut names: Vec<_> = StreamKind::ALL.iter().map(|k| k.table_name_ext()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 6, "6 历史流必须 6 张独立表");
    }

    #[test]
    fn continuity_trait_smoke() {
        let store = SqliteMemoryStore::open_in_memory().unwrap();
        let ep = Episode {
            id: "ep-smoke".into(),
            timestamp: 1_700_000_000,
            role: "user".into(),
            content: "hi".into(),
            session_id: "sess-smoke".into(),
        };
        ContinuitySnapshotStore::put_episode(&store, &ep).unwrap();
        let recent = ContinuitySnapshotStore::recent_episodes(&store, "sess-smoke", 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "ep-smoke");
    }

    #[test]
    fn identity_card_record_roundtrip() {
        let card = IdentityCard {
            continuity_id: "cid-roundtrip".into(),
            birth_time: 1_700_000_000,
            carriers: vec!["carrier-a".into(), "carrier-b".into()],
            migration_history: vec![Migration {
                from_carrier: "carrier-a".into(),
                to_carrier: "carrier-b".into(),
                timestamp: 1_700_000_500,
            }],
        };
        let record = identity::IdentityCardRecord::from_core(&card);
        let back = record.into_core();
        assert_eq!(back.continuity_id, card.continuity_id);
        assert_eq!(back.birth_time, card.birth_time);
        assert_eq!(back.carriers, card.carriers);
        assert_eq!(back.migration_history.len(), 1);
    }

    #[test]
    fn session_and_note_record_roundtrip() {
        let session = Session {
            id: "sess-rt".into(),
            started_at: 1_700_000_000,
            last_active_at: 1_700_000_500,
        };
        let record = session_note::SessionRecord::from_core(&session);
        let back = record.into_core();
        assert_eq!(back.id, session.id);
        assert_eq!(back.started_at, session.started_at);
        assert_eq!(back.last_active_at, session.last_active_at);

        let note = Note {
            id: "n-rt".into(),
            timestamp: 1_700_000_600,
            content: "hello".into(),
            source_episode_ids: vec!["ep-1".into()],
            confidence: 0.7,
            tags: vec!["a".into()],
        };
        let nrecord = session_note::NoteRecord::from_core(&note);
        let nback = nrecord.into_core();
        assert_eq!(nback.content, note.content);
        assert!((nback.confidence - note.confidence).abs() < f64::EPSILON);
    }
}

/// R146: 3 memory crate -> 1 apeireth-memory (子模块)
///
/// dailynote: 按日期分区存储 (R141)
/// layered_memo: 四层闭环记忆系统 (L1 文件 / L2 向量 / L3 标签 / L4 LCM)
pub mod dailynote;
pub mod layered_memo;

/// 编译期守门 (per O-5 不假装)
pub const MEMORY_SUBMODULE_COUNT: usize = 2;

/// P-arch (2026-08-27): 记忆后端抽象 trait.
///
/// **目的**: 摆脱 "apeireth-memory 强绑 SQLite" 的实现细节，
/// 让 domain 操作可插拔后端 (SQLite / File / InMemory / 未来 MongoDB)。
///
/// **架构原则** (per v2-unabsorbed-features.md §A4):
/// - `MemoryBackend` 是 trait，不是具体类型
/// - 具体后端 (sqlite/file/in_memory) 在 `backend` 子模块
/// - 现有 `SqliteMemoryStore` 保留作为 v1 compat facade；新代码走 `Arc<dyn MemoryBackend>`
/// - 0 触碰现有 24 子模块的公共 API（3 不漂移：Episode/Note/Session/IdentityCard/HistoryEntry 签名不变）
///
/// **方法范围**: trait 暴露**跨后端最小集**——append-only 写入 + 列表。
/// 复杂查询 (EpisodeQuery 复合条件) 走具体后端 trait (EpisodeStore) 保留。
pub mod backend;

/// P-arch (2026-08-28): B1 Experience traits, SQLite stores, and conservative
/// evidence-bound extraction (3-layer: Wiki / KG / Association).
///
/// The foundation owns the trait and extractor boundary; this engine owns the
/// SQLite implementations. Full semantic LLM extraction and long-term
/// reflection remain outside the current release.
pub mod experience;
pub mod experience_store_sqlite;
pub mod preference_store;
pub mod preference_store_sqlite;
pub mod self_assessment_store_sqlite;
