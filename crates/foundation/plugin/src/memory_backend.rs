//! P-arch (2026-08-27): MemoryBackend trait (O-6 重构批次 Refactor-1, 撤占位 #17).
//!
//! **位置**: trait 抽象层在 `apeireth-plugin` (foundation), impl 留在 `apeireth-memory` (engine).
//! 与 `CredentialResolver` 同位: 都是 capability 抽象, plugin 管 trait 边界,
//! 业务方管 impl. 单向依赖 (memory → plugin, 不反向).
//!
//! **不重写 SQL**: impl 仍委托现有 `SqliteMemoryStore`, 0 触碰 24 个子模块的 public API.
//!
//! **0 装 PASS**: trait 是 0 装, v2.0.0-rc.1 接真 backend 时实现. 现在仅画边界.
//!
//! **架构最优依据 (O-6 锚 9)**:
//! - 总体: trait 抽象在 foundation 与 ToolCapability/ProviderCapability/CredentialResolver 三件套对齐
//! - 系统: trait 在 foundation, impl 在 engine (单向依赖, 与 plugin 体系一致)
//! - 架构: backend registry 与 plugin registry 同一抽象层, 入口语义不歧义
//!
//! **v1 compat**: `apeireth-memory::backend::MemoryBackend` 通过 re-export 仍可访问,
//! 现有 0 外部 user (15 测试全在 `apeireth-memory` 内部), 0 破坏.
//!
//! **3 阶审查** (commit message 必写明):
//! 1. 总体: 在 v2 整体语境里, 4 个 capability 抽象 (Tool/Provider/CredentialResolver/MemoryBackend) 集中 foundation, 降低 v1 era 86-crate "registry 散在多处" 的风险
//! 2. 系统: trait 在 foundation, impl 在 engine (单向, 与 plugin/Provider/Tool 一致)
//! 3. 架构: 与 plugin manager 单 trait 边界, runtime 拿 `Arc<dyn MemoryBackend>` 注入, 不直接 import memory
//!
//! **O-6 锚 #2 兑现** (2026-08-27): trait method `append_stream` / `list_stream`
//! 从 `&str` + `serde_json::Value` 占位 改为 `apeireth_core::kernel::StreamKind`
//! typed enum + `serde_json::Value`. `StreamKind` 6 变体 canonical 已在 core kernel.
//! HistoryEntry 字段仍走 JSON Value (rc 阶段评估 typed struct 是否值).

use apeireth_core::kernel::{HistoryEntry, StreamKind};
use apeireth_core::Episode;

/// 统一 capability trait 错误类型 (O-6 锚兑现 #12, 2026-08-27).
///
/// 用 std `Box<dyn Error + Send + Sync>` 而非各 backend 本地 error type — 避免
/// plugin → backend crate 循环依赖. impl 端包: `Box::new(my_local_error)`.
///
/// 0 装 PASS: v2.0.0-rc 阶段迁移到 associated `type Error` trait method (impl
/// 自由选具体错误类型) + `From<impl error> for Box<dyn Error>` 自动转换.
pub type CapabilityResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// 后端类型标识。
///
/// 0 假装 PASS：所有 impl 都真实实现，**不**"假装有加密"等尚未落地能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// SQLite（WAL + 现有 migrations）—— 默认后端，向后兼容
    Sqlite,
    /// JSON Lines append-only 文件（明文，encryption 待 v2.1）
    File,
    /// 进程内 HashMap（仅测试，进程重启数据丢失）
    InMemory,
}

impl BackendKind {
    /// 稳定标签字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::File => "file",
            Self::InMemory => "in_memory",
        }
    }
}

/// 记忆后端抽象。
///
/// **核心契约**：
/// - **append-only 写入**：所有 put_* / append_* 操作不可变
/// - **同步 API**：当前 v2 runtime 在 spawn_blocking 里调 memory，sync trait 足够
/// - **Send + Sync**：后端实例可跨线程共享
/// - **错误统一**（O-6 锚 #9 #12 兑现）：所有错误走 `CapabilityResult<T>`
/// - **跨模块类型 (typed enum, O-6 锚 #2 兑现)**: Episode 来自 `apeireth_core`,
///   `StreamKind` 是 `apeireth_core::kernel::StreamKind` typed enum. HistoryEntry 字段仍走
///   `serde_json::Value` (rc 阶段评估 typed struct).
pub trait MemoryBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> BackendKind;
    fn ping(&self) -> CapabilityResult<()> {
        Ok(())
    }

    fn put_episode(&self, ep: &Episode) -> CapabilityResult<()>;
    fn get_episode(&self, id: &str) -> CapabilityResult<Option<Episode>>;
    fn recent_episodes(&self, session_id: &str, n: usize) -> CapabilityResult<Vec<Episode>>;

    /// Persist structured Memory Plane metadata beside an append-only episode.
    ///
    /// This optional extension keeps the foundation trait independent of the
    /// engine's concrete scope types while allowing production backends to
    /// persist scope/provenance additively. Backends without metadata support
    /// retain legacy behavior and return no metadata on reads.
    fn put_episode_metadata(
        &self,
        _episode_id: &str,
        _metadata: serde_json::Value,
    ) -> CapabilityResult<()> {
        Ok(())
    }

    /// Read structured Memory Plane metadata, if the backend supports it.
    fn get_episode_metadata(
        &self,
        _episode_id: &str,
    ) -> CapabilityResult<Option<serde_json::Value>> {
        Ok(None)
    }

    /// 追加一条历史流条目 (append-only). `kind` 是 typed enum (6 流).
    /// `entry` 是 typed `HistoryEntry` struct (O-6 锚 #18 兑现, 替代 serde_json::Value 占位).
    fn append_stream(&self, kind: StreamKind, entry: HistoryEntry) -> CapabilityResult<()>;

    /// 列出某 session 的某流最近 N 条（按时间升序，末尾 N 条，未 tombstone 的）.
    fn list_stream(
        &self,
        kind: StreamKind,
        session_id: &str,
        n: usize,
    ) -> CapabilityResult<Vec<HistoryEntry>>;
}
