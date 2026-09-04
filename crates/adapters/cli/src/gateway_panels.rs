//! Panel data adapter for the gateway introspection surface.
//!
//! Composition-root implementation of the Gateway bounded-context ports
//! (with [`apeireth_gateway::PanelData`] retained only as a legacy adapter):
//! - sessions come from the same durable [`SessionStore`] the runtime uses;
//! - memory episodes come from the same [`MemoryBackend`] the cognitive
//!   modules use; protect/forget are durable governance transitions;
//! - production tools and behavior modules are projected from the live runtime
//!   registries, so the panel cannot advertise a second static universe;
//! - traces and audit are JSONL archives under the data dir; the historical
//!   `memory-flags.jsonl` is read once for migration and is not an authority.
//!
//! Archives are bounded in memory (newest first) and appended to disk
//! best-effort: a full disk must never fail a chat turn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use apeireth_core::Episode;
use apeireth_gateway::{
    AuditCommand, AuditDto, AuditQuery, EpisodeDto, EpisodeMutationDto, GatewayServices,
    GrantCommand, GrantDto, GrantMutationDto, GrantQuery, GraphEdgeDto, GraphNodeDto,
    GuardDryRunRequest, GuardDryRunResponse, GuardEventDto, GuardStatusDto, MemoryCommand,
    MemoryGovernanceCommand, MemoryGraphDto, MemoryQuery, ModuleQuery, OrganDto, PanelData,
    SafetyGuardQuery, SessionQuery, SessionSummaryDto, ToolCatalogQuery, ToolDto, TraceCommand,
    TraceDetailDto, TraceQuery, TraceSpanDto, TraceSummaryDto, WorkbenchMemoryProvenanceDto,
    WorkbenchQuery, WorkbenchToolExecutionDto, WorkbenchTurnDto,
};
use apeireth_governance::{Permission, PermissionPolicy};
use apeireth_memory::{GovernedEpisode, MemoryGovernanceStore};
use apeireth_plugin::memory_backend::MemoryBackend;
use apeireth_protocol::canonical::{ContentPart, MessageRole};
use apeireth_runtime::canonical::{Runtime, Session, SessionStore};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const MAX_TRACES: usize = 500;
const MAX_AUDIT: usize = 1_000;
const TITLE_CHARS: usize = 40;
const GRAPH_EPISODES_PER_SESSION: usize = 200;

fn errstr<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Gateway-level per-episode flags (protect/forget) with a monotonic revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FlagEntry {
    id: String,
    protected: bool,
    tombstoned: bool,
    rev: u64,
}

/// CLI composition backed by the runtime's stores and live projections.
pub struct CliPanelData {
    runtime: Option<Arc<Runtime>>,
    sessions: Arc<dyn SessionStore>,
    memory: Arc<dyn MemoryBackend>,
    /// Authoritative governance service in production. `None` is retained
    /// only for the legacy constructor used by compatibility tests.
    governance: Option<Arc<dyn MemoryGovernanceStore>>,
    policy: Arc<std::sync::Mutex<PermissionPolicy>>,
    tools: Vec<ToolDto>,
    organs: Vec<OrganDto>,
    trace_path: PathBuf,
    audit_path: PathBuf,
    flags_path: PathBuf,
    traces: std::sync::Mutex<Vec<TraceDetailDto>>,
    audit: std::sync::Mutex<Vec<AuditDto>>,
    flags: std::sync::Mutex<HashMap<String, FlagEntry>>,
    guard_hook: Option<Arc<apeireth_guard::BehaviorChainGuardHook>>,
}

impl CliPanelData {
    /// Build over the shared stores and archive files in `data_dir`.
    pub fn new(
        sessions: Arc<dyn SessionStore>,
        memory: Arc<dyn MemoryBackend>,
        policy: Arc<std::sync::Mutex<PermissionPolicy>>,
        enable_local_read_tools: bool,
        data_dir: PathBuf,
    ) -> Self {
        let _ = std::fs::create_dir_all(&data_dir);
        let trace_path = data_dir.join("traces.jsonl");
        let audit_path = data_dir.join("daemon-audit.jsonl");
        let flags_path = data_dir.join("memory-flags.jsonl");

        let traces = load_newest_first::<TraceDetailDto>(&trace_path, MAX_TRACES);
        let audit = load_newest_first::<AuditDto>(&audit_path, MAX_AUDIT);
        let flags = load_flags(&flags_path);

        Self {
            runtime: None,
            sessions,
            memory,
            governance: None,
            policy,
            tools: legacy_compatibility_tool_catalog(enable_local_read_tools),
            organs: legacy_compatibility_behavior_catalog(),
            trace_path,
            audit_path,
            flags_path,
            traces: std::sync::Mutex::new(traces),
            audit: std::sync::Mutex::new(audit),
            flags: std::sync::Mutex::new(flags),
            guard_hook: None,
        }
    }

    /// Attach a safety guard hook for telemetry and dry-run queries.
    pub fn with_guard(mut self, guard: Arc<apeireth_guard::BehaviorChainGuardHook>) -> Self {
        self.guard_hook = Some(guard);
        self
    }

    /// Build the production panel projection from the live runtime assembly.
    ///
    /// The legacy constructor remains available for embedders that only have
    /// stores. Production gateway wiring should use this constructor so tool
    /// and behavior projections follow the same registries as execution.
    pub fn new_with_runtime(
        runtime: Arc<Runtime>,
        sessions: Arc<dyn SessionStore>,
        memory: Arc<dyn MemoryBackend>,
        governance: Arc<dyn MemoryGovernanceStore>,
        policy: Arc<std::sync::Mutex<PermissionPolicy>>,
        enable_local_read_tools: bool,
        data_dir: PathBuf,
    ) -> Self {
        let mut panel = Self::new_with_governance(
            sessions,
            memory,
            governance,
            policy,
            enable_local_read_tools,
            data_dir,
        );
        panel.tools = project_live_tool_catalog(&runtime, &panel.policy);
        panel.organs = project_live_behavior_catalog(&runtime);
        panel.runtime = Some(runtime);
        panel
    }

    /// Build the production panel adapter over the same episode governance
    /// service used by runtime recall. The old JSONL flags file is read only
    /// once for migration and never used for subsequent decisions.
    pub fn new_with_governance(
        sessions: Arc<dyn SessionStore>,
        memory: Arc<dyn MemoryBackend>,
        governance: Arc<dyn MemoryGovernanceStore>,
        policy: Arc<std::sync::Mutex<PermissionPolicy>>,
        enable_local_read_tools: bool,
        data_dir: PathBuf,
    ) -> Self {
        let mut panel = Self::new(sessions, memory, policy, enable_local_read_tools, data_dir);
        let legacy_flags =
            std::mem::take(&mut *panel.flags.lock().expect("legacy memory flags mutex"));
        migrate_legacy_flags(&governance, legacy_flags);
        panel.governance = Some(governance);
        panel.flags_path = PathBuf::new();
        panel
    }

    fn episode_dto(&self, episode: &Episode, flags: &HashMap<String, FlagEntry>) -> EpisodeDto {
        let flag = flags.get(&episode.id);
        EpisodeDto {
            id: episode.id.clone(),
            // core episodes store epoch seconds; the contract is epoch ms.
            timestamp: episode.timestamp.saturating_mul(1_000),
            role: episode.role.clone(),
            content: episode.content.clone(),
            session_id: episode.session_id.clone(),
            category: None,
            importance: None,
            protected: Some(flag.map(|f| f.protected).unwrap_or(false)),
            status: Some("active".to_string()),
        }
    }

    fn governed_episode_dto(&self, governed: GovernedEpisode) -> EpisodeDto {
        EpisodeDto {
            id: governed.episode.id,
            timestamp: governed.episode.timestamp.saturating_mul(1_000),
            role: governed.episode.role,
            content: governed.episode.content,
            session_id: governed.episode.session_id,
            category: None,
            importance: None,
            protected: Some(governed.protected),
            status: Some(governed.status.as_str().to_string()),
        }
    }

    /// Apply a protect/unprotect/forget transition under an optimistic
    /// revision check; persist the new flag entry on success.
    fn mutate_flag(
        &self,
        id: &str,
        expected_rev: u64,
        apply: impl FnOnce(&mut FlagEntry),
    ) -> Result<EpisodeMutationDto, String> {
        let mut flags = self.flags.lock().map_err(errstr)?;
        let entry = flags
            .get_mut(id)
            .ok_or_else(|| format!("episode {id} not found"))?;
        if entry.rev != expected_rev {
            return Err(format!(
                "revision conflict: expected {expected_rev}, current {}",
                entry.rev
            ));
        }
        apply(entry);
        entry.rev += 1;
        let updated = entry.clone();
        drop(flags);
        append_jsonl(&self.flags_path, &updated);
        Ok(EpisodeMutationDto {
            ok: true,
            rev: updated.rev,
            id: updated.id,
            status: if updated.tombstoned {
                "forgotten".into()
            } else {
                "active".into()
            },
            protected: updated.protected,
            revision: updated.rev,
            content: String::new(),
        })
    }

    fn mutation_dto(governed: GovernedEpisode) -> EpisodeMutationDto {
        EpisodeMutationDto {
            ok: true,
            rev: governed.revision as u64,
            id: governed.episode.id,
            status: governed.status.as_str().to_string(),
            protected: governed.protected,
            revision: governed.revision as u64,
            content: governed.episode.content,
        }
    }
}

fn migrate_legacy_flags(
    governance: &Arc<dyn MemoryGovernanceStore>,
    flags: HashMap<String, FlagEntry>,
) {
    for flag in flags.into_values() {
        let Ok(Some(current)) = governance.get_governed(&flag.id) else {
            continue;
        };
        // A non-zero revision is newer durable state; never overwrite it with
        // a stale JSONL snapshot.
        if current.revision != 0 {
            continue;
        }
        let mut revision = current.revision;
        if flag.protected && !current.protected {
            match governance.protect_episode(&flag.id, revision) {
                Ok(updated) => revision = updated.revision,
                Err(_) => continue,
            }
        }
        if flag.tombstoned {
            let _ = governance.forget_episode(&flag.id, None, revision);
        }
    }
}

/// Legacy compatibility catalog for embedders that do not supply a Runtime.
/// Production uses [`project_live_tool_catalog`] instead.
fn legacy_compatibility_tool_catalog(enable_local_read_tools: bool) -> Vec<ToolDto> {
    let local = |name: &str, description: &str| ToolDto {
        name: name.to_string(),
        description: description.to_string(),
        args_schema: None,
        source: "builtin".to_string(),
        permission: if enable_local_read_tools {
            "granted"
        } else {
            "none"
        }
        .to_string(),
        available: enable_local_read_tools,
    };
    vec![
        ToolDto {
            name: "tool.repo".to_string(),
            description: "仓库检查: git 状态 / 提交历史 / 差异 (只读)".to_string(),
            args_schema: None,
            source: "builtin".to_string(),
            permission: "granted".to_string(),
            available: true,
        },
        local(
            "tool.filesystem",
            "受控文件读取 (仅工作区根目录内, 敏感路径拒绝)",
        ),
        local("tool.search", "工作区内容检索 (文件名与全文, 结果限量)"),
    ]
}

/// Legacy compatibility catalog for embedders that do not supply a Runtime.
/// Production uses [`project_live_behavior_catalog`] instead.
fn legacy_compatibility_behavior_catalog() -> Vec<OrganDto> {
    let organ = |id: &str, name: &str, description: &str| OrganDto {
        id: id.to_string(),
        name: name.to_string(),
        enabled: false,
        description: Some(description.to_string()),
    };
    vec![
        organ("W1", "World Model", "世界模型校准与前向预测"),
        organ(
            "W2",
            "Causal World Model",
            "因果世界模型 (CoW 分支 + SAGA 补偿)",
        ),
        organ("W3", "Causal Edge Mining", "因果边挖掘"),
        organ("E4", "Curiosity", "好奇驱动探索"),
        organ("F4", "Hypothesis", "假设提出与验证计划"),
        organ("F1", "Emotion Memory", "情感记忆"),
        organ("F6", "Value Cases", "价值案例库"),
        organ("E7", "Emergence", "涌现行为观察"),
        organ("Memory", "Memory Merger", "跨流记忆合并"),
    ]
}

fn project_live_tool_catalog(
    runtime: &Runtime,
    policy: &std::sync::Mutex<PermissionPolicy>,
) -> Vec<ToolDto> {
    let policy = policy.lock().ok();
    let mut tools = runtime
        .capability_registry()
        .entries()
        .into_iter()
        .map(|(owner, capability)| {
            let declaration = capability.declaration();
            let capability_id = capability.id().to_string();
            let granted = policy.as_ref().is_some_and(|policy| {
                policy.iter().any(|permission| {
                    matches!(permission, Permission::ExecuteTool(name) if name == &capability_id || name == &declaration.name)
                })
            });
            ToolDto {
                name: declaration.name,
                description: declaration.description.unwrap_or_default(),
                args_schema: (!declaration.parameters.is_empty())
                    .then(|| serde_json::Value::Object(declaration.parameters)),
                source: owner,
                permission: if granted { "granted" } else { "none" }.to_string(),
                available: true,
            }
        })
        .collect::<Vec<_>>();

    for capability in runtime.plugins().active_tools() {
        let declaration = capability.declaration();
        tools.push(ToolDto {
            name: declaration.name,
            description: declaration.description.unwrap_or_default(),
            args_schema: (!declaration.parameters.is_empty())
                .then(|| serde_json::Value::Object(declaration.parameters)),
            source: "plugin".to_string(),
            permission: "plugin-managed".to_string(),
            available: true,
        });
    }
    tools
}

fn project_live_behavior_catalog(runtime: &Runtime) -> Vec<OrganDto> {
    runtime
        .modules()
        .iter()
        .map(|module| OrganDto {
            id: module.manifest().id.clone(),
            name: module.manifest().name.clone(),
            enabled: true,
            description: Some("runtime behavior module".to_string()),
        })
        .collect()
}

fn grant_capability(permission: &Permission) -> String {
    match permission {
        Permission::ExecuteTool(name) => name.clone(),
        Permission::ReadMemory => "memory.read".to_string(),
        Permission::WriteMemory => "memory.write".to_string(),
        Permission::NetworkEgress(scope) => scope.clone(),
        Permission::ModifyIdentity => "identity.modify".to_string(),
        Permission::AdminOverride => "admin.override".to_string(),
    }
}

fn session_title(session: &Session) -> Option<String> {
    session
        .messages
        .iter()
        .find(|m| m.role == MessageRole::User)
        .map(|m| ContentPart::join_text(&m.content))
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .map(|text| snippet(&text, TITLE_CHARS))
}

fn snippet(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Load JSONL newest-first (lines are appended chronologically), skipping
/// unparseable legacy rows, bounded to `cap`.
fn load_newest_first<T: serde::de::DeserializeOwned>(path: &PathBuf, cap: usize) -> Vec<T> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut parsed: Vec<T> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<T>(line).ok())
        .collect();
    if parsed.len() > cap {
        parsed.drain(0..parsed.len() - cap);
    }
    parsed.reverse();
    parsed
}

/// Load the flag archive; later lines win (append-only updates).
fn load_flags(path: &PathBuf) -> HashMap<String, FlagEntry> {
    let mut flags = HashMap::new();
    if let Ok(raw) = std::fs::read_to_string(path) {
        for line in raw.lines() {
            if let Ok(entry) = serde_json::from_str::<FlagEntry>(line) {
                flags.insert(entry.id.clone(), entry);
            }
        }
    }
    flags
}

fn append_jsonl<T: serde::Serialize>(path: &PathBuf, value: &T) {
    let Ok(line) = serde_json::to_string(value) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[async_trait]
impl PanelData for CliPanelData {
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryDto>, String> {
        let sessions = self.sessions.list().await.map_err(errstr)?;
        Ok(sessions
            .into_iter()
            .map(|session| SessionSummaryDto {
                id: session.id.to_string(),
                title: session_title(&session),
                created_at: session.created_at.epoch_millis(),
                updated_at: session.updated_at.epoch_millis(),
                message_count: session.messages.len(),
                revision: session.revision,
            })
            .collect())
    }

    async fn list_tools(&self) -> Result<Vec<ToolDto>, String> {
        if let Some(runtime) = &self.runtime {
            // Re-project on every read so dynamic MCP registration/unregister
            // is visible immediately through the Gateway catalog.
            return Ok(project_live_tool_catalog(runtime, &self.policy));
        }
        Ok(self.tools.clone())
    }

    async fn list_traces(&self, limit: usize) -> Result<Vec<TraceSummaryDto>, String> {
        let traces = self.traces.lock().map_err(errstr)?;
        Ok(traces
            .iter()
            .take(limit)
            .filter_map(|detail| {
                let root = detail.spans.first()?.clone();
                Some(TraceSummaryDto {
                    trace_id: detail.trace_id.clone(),
                    span_count: detail.spans.len(),
                    started_at: root.started_at,
                    root_span: root,
                })
            })
            .collect())
    }

    async fn trace_detail(&self, trace_id: &str) -> Result<Option<TraceDetailDto>, String> {
        let traces = self.traces.lock().map_err(errstr)?;
        Ok(traces
            .iter()
            .find(|detail| detail.trace_id == trace_id)
            .cloned())
    }

    async fn list_audit(&self, limit: usize) -> Result<Vec<AuditDto>, String> {
        let audit = self.audit.lock().map_err(errstr)?;
        Ok(audit.iter().take(limit).cloned().collect())
    }

    async fn append_audit(&self, event: &str, detail: Option<&str>) {
        let entry = AuditDto {
            ts: now_millis(),
            event: event.to_string(),
            service: "gateway".to_string(),
            detail: detail.map(|d| d.to_string()),
        };
        append_jsonl(&self.audit_path, &entry);
        if let Ok(mut audit) = self.audit.lock() {
            audit.insert(0, entry);
            audit.truncate(MAX_AUDIT);
        }
    }

    async fn append_trace(&self, trace_id: &str, spans: Vec<TraceSpanDto>) {
        if spans.is_empty() {
            return;
        }
        let detail = TraceDetailDto {
            trace_id: trace_id.to_string(),
            spans,
        };
        append_jsonl(&self.trace_path, &detail);
        if let Ok(mut traces) = self.traces.lock() {
            traces.retain(|existing| existing.trace_id != detail.trace_id);
            traces.insert(0, detail);
            traces.truncate(MAX_TRACES);
        }
    }

    fn supports_memory(&self) -> bool {
        true
    }

    async fn list_episodes(
        &self,
        session: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EpisodeDto>, String> {
        if let Some(governance) = &self.governance {
            let mut governed = Vec::new();
            if let Some(session_id) = session {
                governed.extend(
                    governance
                        .governed_recent_episodes(session_id, limit)
                        .map_err(errstr)?,
                );
            } else {
                for stored in &self.sessions.list().await.map_err(errstr)? {
                    governed.extend(
                        governance
                            .governed_recent_episodes(&stored.id.to_string(), limit)
                            .map_err(errstr)?,
                    );
                }
            }
            let query_lc = query.map(str::to_lowercase);
            let mut out = governed
                .into_iter()
                .filter(|episode| {
                    query_lc
                        .as_ref()
                        .is_none_or(|value| episode.episode.content.to_lowercase().contains(value))
                })
                .map(|episode| self.governed_episode_dto(episode))
                .collect::<Vec<_>>();
            out.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
            out.truncate(limit);
            return Ok(out);
        }
        let mut raw: Vec<Episode> = Vec::new();
        if let Some(session_id) = session {
            raw = self
                .memory
                .recent_episodes(session_id, limit)
                .map_err(errstr)?;
        } else {
            let sessions = self.sessions.list().await.map_err(errstr)?;
            for stored in &sessions {
                let mut recents = self
                    .memory
                    .recent_episodes(&stored.id.to_string(), limit)
                    .map_err(errstr)?;
                raw.append(&mut recents);
            }
        }

        let flags = self.flags.lock().map_err(errstr)?;
        let query_lc = query.map(|q| q.to_lowercase());
        let mut out: Vec<EpisodeDto> = Vec::new();
        for episode in raw {
            if let Some(flag) = flags.get(&episode.id) {
                if flag.tombstoned {
                    continue;
                }
            }
            if let Some(q) = &query_lc {
                if !episode.content.to_lowercase().contains(q) {
                    continue;
                }
            }
            out.push(self.episode_dto(&episode, &flags));
        }
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        out.truncate(limit);
        Ok(out)
    }

    async fn append_episode(
        &self,
        session: &str,
        role: &str,
        content: &str,
    ) -> Result<EpisodeDto, String> {
        // A session that owns memory must exist in the session ledger, so the
        // global episode list (which enumerates sessions) can reach it.
        let session_id: apeireth_core::kernel::SessionId = session.parse().map_err(errstr)?;
        if self
            .sessions
            .load(&session_id)
            .await
            .map_err(errstr)?
            .is_none()
        {
            let clock = apeireth_core::kernel::system_clock();
            let created = Session::new(session_id, clock.as_ref());
            self.sessions.save(&created).await.map_err(errstr)?;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let episode = Episode {
            id: id.clone(),
            timestamp: now_millis() / 1_000,
            role: role.to_string(),
            content: content.to_string(),
            session_id: session.to_string(),
        };
        self.memory.put_episode(&episode).map_err(errstr)?;
        if let Some(governance) = &self.governance {
            return governance
                .get_governed(&id)
                .map_err(errstr)?
                .map(|episode| self.governed_episode_dto(episode))
                .ok_or_else(|| format!("episode {id} was not visible after append"));
        }
        let flag = FlagEntry {
            id: id.clone(),
            protected: false,
            tombstoned: false,
            rev: 0,
        };
        append_jsonl(&self.flags_path, &flag);
        if let Ok(mut flags) = self.flags.lock() {
            flags.insert(id.clone(), flag);
        }
        Ok(EpisodeDto {
            id,
            timestamp: episode.timestamp.saturating_mul(1_000),
            role: episode.role,
            content: episode.content,
            session_id: episode.session_id,
            category: None,
            importance: None,
            protected: Some(false),
            status: Some("active".to_string()),
        })
    }

    async fn protect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        if let Some(governance) = &self.governance {
            let episode = governance
                .protect_episode(id, expected_rev as i64)
                .map_err(errstr)?;
            return Ok(Self::mutation_dto(episode));
        }
        self.mutate_flag(id, expected_rev, |entry| entry.protected = true)
    }

    async fn unprotect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        if let Some(governance) = &self.governance {
            let episode = governance
                .unprotect_episode(id, expected_rev as i64)
                .map_err(errstr)?;
            return Ok(Self::mutation_dto(episode));
        }
        self.mutate_flag(id, expected_rev, |entry| entry.protected = false)
    }

    async fn forget_episode(
        &self,
        id: &str,
        expected_rev: u64,
        _reason: Option<&str>,
    ) -> Result<EpisodeMutationDto, String> {
        if let Some(governance) = &self.governance {
            let episode = governance
                .forget_episode(id, _reason, expected_rev as i64)
                .map_err(errstr)?;
            return Ok(Self::mutation_dto(episode));
        }
        self.mutate_flag(id, expected_rev, |entry| entry.tombstoned = true)
    }

    async fn memory_graph(&self) -> Result<MemoryGraphDto, String> {
        let sessions = self.sessions.list().await.map_err(errstr)?;
        if let Some(governance) = &self.governance {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();
            for stored in &sessions {
                let session_node = format!("session:{}", stored.id);
                nodes.push(GraphNodeDto {
                    id: session_node.clone(),
                    label: session_title(stored).unwrap_or_else(|| stored.id.to_string()),
                    kind: "session".to_string(),
                });
                for episode in governance
                    .governed_recent_episodes(&stored.id.to_string(), GRAPH_EPISODES_PER_SESSION)
                    .map_err(errstr)?
                {
                    nodes.push(GraphNodeDto {
                        id: episode.episode.id.clone(),
                        label: snippet(&episode.episode.content, TITLE_CHARS),
                        kind: "episode".to_string(),
                    });
                    edges.push(GraphEdgeDto {
                        from: session_node.clone(),
                        to: episode.episode.id,
                        weight: 1.0,
                        label: None,
                    });
                }
            }
            return Ok(MemoryGraphDto { nodes, edges });
        }
        let flags = self.flags.lock().map_err(errstr)?;
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for stored in &sessions {
            let session_node = format!("session:{}", stored.id);
            nodes.push(GraphNodeDto {
                id: session_node.clone(),
                label: session_title(stored).unwrap_or_else(|| stored.id.to_string()),
                kind: "session".to_string(),
            });
            let recents = self
                .memory
                .recent_episodes(&stored.id.to_string(), GRAPH_EPISODES_PER_SESSION)
                .map_err(errstr)?;
            for episode in recents {
                if let Some(flag) = flags.get(&episode.id) {
                    if flag.tombstoned {
                        continue;
                    }
                }
                nodes.push(GraphNodeDto {
                    id: episode.id.clone(),
                    label: snippet(&episode.content, TITLE_CHARS),
                    kind: "episode".to_string(),
                });
                edges.push(GraphEdgeDto {
                    from: session_node.clone(),
                    to: episode.id.clone(),
                    weight: 1.0,
                    label: None,
                });
            }
        }
        Ok(MemoryGraphDto { nodes, edges })
    }

    fn supports_permissions(&self) -> bool {
        true
    }

    async fn list_grants(&self) -> Result<Vec<GrantDto>, String> {
        let policy = self.policy.lock().map_err(errstr)?;
        Ok(policy
            .iter()
            .map(|permission| GrantDto {
                permission: permission.label(),
                capability: grant_capability(permission),
                granted_at: None,
            })
            .collect())
    }

    async fn revoke_grant(&self, capability: &str) -> Result<GrantMutationDto, String> {
        let permission = match capability {
            "memory.read" => Permission::ReadMemory,
            "memory.write" => Permission::WriteMemory,
            "identity.modify" => Permission::ModifyIdentity,
            "admin.override" => Permission::AdminOverride,
            other => Permission::ExecuteTool(other.to_string()),
        };
        let mut policy = self.policy.lock().map_err(errstr)?;
        let ok = policy.revoke(&permission);
        Ok(GrantMutationDto { ok })
    }

    fn supports_organs(&self) -> bool {
        true
    }

    async fn list_organs(&self) -> Result<Vec<OrganDto>, String> {
        if let Some(runtime) = &self.runtime {
            return Ok(project_live_behavior_catalog(runtime));
        }
        Ok(self.organs.clone())
    }
}

// The production gateway consumes these narrow ports instead of the legacy
// all-in-one PanelData trait. Each adapter delegates to the same implementation
// above, so the HTTP projections and compatibility tests share one source.

#[async_trait]
impl SessionQuery for CliPanelData {
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryDto>, String> {
        PanelData::list_sessions(self).await
    }
}

#[async_trait]
impl MemoryQuery for CliPanelData {
    async fn list_episodes(
        &self,
        session: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EpisodeDto>, String> {
        PanelData::list_episodes(self, session, query, limit).await
    }

    async fn memory_graph(&self) -> Result<MemoryGraphDto, String> {
        PanelData::memory_graph(self).await
    }
}

#[async_trait]
impl MemoryCommand for CliPanelData {
    async fn append_episode(
        &self,
        session: &str,
        role: &str,
        content: &str,
    ) -> Result<EpisodeDto, String> {
        PanelData::append_episode(self, session, role, content).await
    }
}

#[async_trait]
impl MemoryGovernanceCommand for CliPanelData {
    async fn protect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        PanelData::protect_episode(self, id, expected_rev).await
    }

    async fn unprotect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        PanelData::unprotect_episode(self, id, expected_rev).await
    }

    async fn forget_episode(
        &self,
        id: &str,
        expected_rev: u64,
        reason: Option<&str>,
    ) -> Result<EpisodeMutationDto, String> {
        PanelData::forget_episode(self, id, expected_rev, reason).await
    }
}

#[async_trait]
impl ToolCatalogQuery for CliPanelData {
    async fn list_tools(&self) -> Result<Vec<ToolDto>, String> {
        PanelData::list_tools(self).await
    }
}

#[async_trait]
impl TraceQuery for CliPanelData {
    async fn list_traces(&self, limit: usize) -> Result<Vec<TraceSummaryDto>, String> {
        PanelData::list_traces(self, limit).await
    }

    async fn trace_detail(&self, trace_id: &str) -> Result<Option<TraceDetailDto>, String> {
        PanelData::trace_detail(self, trace_id).await
    }
}

#[async_trait]
impl TraceCommand for CliPanelData {
    async fn append_trace(&self, trace_id: &str, spans: Vec<TraceSpanDto>) {
        PanelData::append_trace(self, trace_id, spans).await
    }
}

#[async_trait]
impl AuditQuery for CliPanelData {
    async fn list_audit(&self, limit: usize) -> Result<Vec<AuditDto>, String> {
        PanelData::list_audit(self, limit).await
    }
}

#[async_trait]
impl AuditCommand for CliPanelData {
    async fn append_audit(&self, event: &str, detail: Option<&str>) {
        PanelData::append_audit(self, event, detail).await
    }
}

#[async_trait]
impl GrantQuery for CliPanelData {
    async fn list_grants(&self) -> Result<Vec<GrantDto>, String> {
        PanelData::list_grants(self).await
    }
}

#[async_trait]
impl GrantCommand for CliPanelData {
    async fn revoke_grant(&self, capability: &str) -> Result<GrantMutationDto, String> {
        PanelData::revoke_grant(self, capability).await
    }
}

#[async_trait]
impl ModuleQuery for CliPanelData {
    async fn list_modules(&self) -> Result<Vec<OrganDto>, String> {
        PanelData::list_organs(self).await
    }
}

#[async_trait]
impl SafetyGuardQuery for CliPanelData {
    async fn status(&self) -> Result<GuardStatusDto, String> {
        let Some(guard) = &self.guard_hook else {
            return Err("safety guard not configured".into());
        };
        Ok(guard.status())
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<GuardEventDto>, String> {
        let Some(guard) = &self.guard_hook else {
            return Err("safety guard not configured".into());
        };
        Ok(guard.recent_events(Some(limit)))
    }

    async fn dry_run(&self, req: GuardDryRunRequest) -> Result<GuardDryRunResponse, String> {
        let Some(guard) = &self.guard_hook else {
            return Err("safety guard not configured".into());
        };
        Ok(guard.dry_run(&req))
    }
}

#[async_trait]
impl WorkbenchQuery for CliPanelData {
    async fn turn_status(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<WorkbenchTurnDto>, String> {
        let session = match session_id {
            Some(id) => match id.parse::<apeireth_core::kernel::SessionId>() {
                Ok(sid) => self.sessions.load(&sid).await.map_err(errstr)?,
                Err(_) => None,
            },
            None => {
                let all = self.sessions.list().await.map_err(errstr)?;
                all.into_iter().max_by_key(|s| s.updated_at.epoch_millis())
            }
        };

        let Some(session) = session else {
            return Ok(None);
        };

        let goal = session_title(&session).unwrap_or_else(|| "新会话".to_string());
        let session_str = session.id.to_string();

        let traces = self.traces.lock().map_err(errstr)?;
        let trace = traces
            .iter()
            .find(|t| {
                t.spans
                    .iter()
                    .any(|s| s.session_id.as_deref() == Some(&session_str))
            })
            .or_else(|| traces.first());

        let mut tools = Vec::new();
        let mut recalled_count = 0;
        let mut layers = Vec::new();

        if let Some(trace) = trace {
            for span in &trace.spans {
                if span.kind == "tool" || span.actor == "tool" || span.actor == "capability" {
                    let latency = span
                        .ended_at
                        .map(|end| end.saturating_sub(span.started_at) as u64);
                    tools.push(WorkbenchToolExecutionDto {
                        id: span.span_id.clone(),
                        name: span.summary.clone().unwrap_or_else(|| span.actor.clone()),
                        status: span.status.clone(),
                        latency_ms: latency,
                        error: if span.status == "error" || span.status == "failed" {
                            Some("tool execution failed".into())
                        } else {
                            None
                        },
                    });
                }
                if span.actor == "memory" || span.kind == "memory_recall" {
                    recalled_count += 1;
                    if !layers.contains(&span.actor) {
                        layers.push(span.actor.clone());
                    }
                }
            }
        }

        if layers.is_empty() {
            layers.push("episodic".to_string());
            layers.push("working".to_string());
        }

        let mut guard_verdict = None;
        if let Some(guard) = &self.guard_hook {
            let events = guard.recent_events(Some(20));
            if let Some(ev) = events.iter().find(|e| e.session_id == session_str) {
                guard_verdict = Some(ev.decision.clone());
            }
        }

        Ok(Some(WorkbenchTurnDto {
            session_id: session.id.to_string(),
            goal,
            agent_status: "idle".to_string(),
            tools,
            memory: WorkbenchMemoryProvenanceDto {
                recalled_count,
                governance_filtered: 0,
                layers,
            },
            guard_verdict,
            updated_at: session.updated_at.epoch_millis(),
        }))
    }
}

/// Build the gateway service graph from one live CLI composition root.
pub fn gateway_services(panel: Arc<CliPanelData>) -> GatewayServices {
    let safety_guard: Option<Arc<dyn SafetyGuardQuery>> = if panel.guard_hook.is_some() {
        Some(panel.clone() as Arc<dyn SafetyGuardQuery>)
    } else {
        None
    };
    GatewayServices {
        sessions: Some(panel.clone() as Arc<dyn SessionQuery>),
        memory: Some(panel.clone() as Arc<dyn MemoryQuery>),
        memory_commands: Some(panel.clone() as Arc<dyn MemoryCommand>),
        memory_governance: Some(panel.clone() as Arc<dyn MemoryGovernanceCommand>),
        tools: Some(panel.clone() as Arc<dyn ToolCatalogQuery>),
        traces: Some(panel.clone() as Arc<dyn TraceQuery>),
        trace_commands: Some(panel.clone() as Arc<dyn TraceCommand>),
        audit: Some(panel.clone() as Arc<dyn AuditQuery>),
        audit_commands: Some(panel.clone() as Arc<dyn AuditCommand>),
        grants: Some(panel.clone() as Arc<dyn GrantQuery>),
        grant_commands: Some(panel.clone() as Arc<dyn GrantCommand>),
        modules: Some(panel.clone() as Arc<dyn ModuleQuery>),
        safety_guard,
        workbench: Some(panel as Arc<dyn WorkbenchQuery>),
    }
}
