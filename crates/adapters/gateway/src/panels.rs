//! Panel introspection surface (`/v1/panel/*`, `/v1/tools/list`, `/v1/apeireth/capabilities`).
//!
//! The gateway owns HTTP shape and transport only; concrete data access is
//! supplied by the composition root through bounded-context ports in
//! [`GatewayServices`]. When no service is configured (e.g. embedded tests), every panel route answers
//! `501 unsupported` with the canonical error body — the frontend treats that
//! as honest degradation, never as a transport failure.
//!
//! Response shapes follow `docs/gateway-api-contract.md` §4-§9 and mirror the
//! desktop types in the sibling `apeireth-ui/src/lib/types.ts` workspace.

use std::sync::Arc;

use apeireth_runtime::canonical::Runtime;
use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// Shared router state: the runtime, optional panel backends, and the
/// gateway-level SSE event bus.
#[derive(Clone)]
pub struct GatewayState {
    pub runtime: Arc<Runtime>,
    pub services: GatewayServices,
    pub events: crate::events::EventBus,
    pub observations: Arc<crate::events::RuntimeObservationSink>,
}

// ---------------------------------------------------------------------------
// DTOs (stable contract — do not rename fields without updating the contract doc)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummaryDto {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSpanDto {
    pub span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub kind: String,
    pub actor: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSummaryDto {
    pub trace_id: String,
    pub span_count: usize,
    pub started_at: i64,
    pub root_span: TraceSpanDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDetailDto {
    pub trace_id: String,
    pub spans: Vec<TraceSpanDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDto {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_schema: Option<serde_json::Value>,
    pub source: String,
    pub permission: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditDto {
    pub ts: i64,
    pub event: String,
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EpisodeDto {
    pub id: String,
    /// Epoch milliseconds (adapter converts the core seconds representation).
    pub timestamp: i64,
    pub role: String,
    pub content: String,
    pub session_id: String,
    /// Omitted when the backend does not store the field (rc honesty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EpisodeMutationDto {
    pub ok: bool,
    /// Compatibility alias retained for older desktop clients.
    pub rev: u64,
    pub id: String,
    pub status: String,
    pub protected: bool,
    pub revision: u64,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNodeDto {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdgeDto {
    pub from: String,
    pub to: String,
    pub weight: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryGraphDto {
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<GraphEdgeDto>,
}

pub use apeireth_guard::{GuardDryRunRequest, GuardDryRunResponse, GuardEventDto, GuardStatusDto};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchToolExecutionDto {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchMemoryProvenanceDto {
    pub recalled_count: usize,
    pub governance_filtered: usize,
    pub layers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchTurnDto {
    pub session_id: String,
    pub goal: String,
    pub agent_status: String,
    pub tools: Vec<WorkbenchToolExecutionDto>,
    pub memory: WorkbenchMemoryProvenanceDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard_verdict: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrantDto {
    /// Stable permission label, e.g. `execute_tool:tool.repo`.
    pub permission: String,
    /// Capability id this grant governs, e.g. `tool.repo`.
    pub capability: String,
    /// Omitted: the canonical policy does not timestamp grants (rc honesty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrantMutationDto {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrganDto {
    /// Stable organ code, e.g. `W1`.
    pub id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// PanelData — the composition-root contract
// ---------------------------------------------------------------------------

/// Read/write handles for the introspection panels.
///
/// Implementations live in the composition root (the CLI adapts its session
/// store, tool catalog and trace/audit archives). All list methods return
/// newest-first. Errors are rendered as `500 runtime_error` with the message
/// as-is; implementations must not leak secrets into error strings.
#[async_trait]
pub trait PanelData: Send + Sync {
    /// Session summaries, most recently updated first.
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryDto>, String>;

    /// Tool catalog with permission annotations.
    async fn list_tools(&self) -> Result<Vec<ToolDto>, String>;

    /// Recent trace summaries, newest first.
    async fn list_traces(&self, limit: usize) -> Result<Vec<TraceSummaryDto>, String>;

    /// Full span list for one trace, or `None` when unknown.
    async fn trace_detail(&self, trace_id: &str) -> Result<Option<TraceDetailDto>, String>;

    /// Recent audit events, newest first.
    async fn list_audit(&self, limit: usize) -> Result<Vec<AuditDto>, String>;

    /// Best-effort audit append (chat/approval lifecycle). Never fails the turn.
    async fn append_audit(&self, event: &str, detail: Option<&str>);

    /// Best-effort trace archive append after a completed turn.
    async fn append_trace(&self, trace_id: &str, spans: Vec<TraceSpanDto>);

    /// Whether the memory introspection surface is available. Drives the
    /// capability manifest; defaults to `false` so a minimal embedder stays
    /// honest about what it can serve.
    fn supports_memory(&self) -> bool {
        false
    }

    /// Episodes, newest first, optionally filtered by session and/or a
    /// case-insensitive content query. Tombstoned episodes are omitted.
    async fn list_episodes(
        &self,
        session: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EpisodeDto>, String>;

    /// Append one episode and return its DTO (rev starts at 0).
    async fn append_episode(
        &self,
        session: &str,
        role: &str,
        content: &str,
    ) -> Result<EpisodeDto, String>;

    /// Gateway-level protect/unprotect/forget flags with optimistic revision
    /// checks. `expected_rev` must equal the current revision, otherwise a
    /// conflict error is returned and nothing changes.
    async fn protect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String>;
    async fn unprotect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String>;
    async fn forget_episode(
        &self,
        id: &str,
        expected_rev: u64,
        reason: Option<&str>,
    ) -> Result<EpisodeMutationDto, String>;

    /// Memory graph (v1 semantics: session nodes + episode nodes with
    /// containment edges derived from real stored data).
    async fn memory_graph(&self) -> Result<MemoryGraphDto, String>;

    /// Whether the permissions introspection surface (grants list / hot
    /// revoke) is available. Defaults to `false`.
    fn supports_permissions(&self) -> bool {
        false
    }

    /// Current grants, deterministic order.
    async fn list_grants(&self) -> Result<Vec<GrantDto>, String>;

    /// Revoke the grant governing `capability`. Session-scoped hot change:
    /// process restart restores the default policy.
    async fn revoke_grant(&self, capability: &str) -> Result<GrantMutationDto, String>;

    /// Whether an organ catalog is available. Defaults to `false`.
    fn supports_organs(&self) -> bool {
        false
    }

    /// The organ catalog (production default: organs chain disabled).
    async fn list_organs(&self) -> Result<Vec<OrganDto>, String>;
}

// ---------------------------------------------------------------------------
// Bounded-context gateway services
// ---------------------------------------------------------------------------

/// Session read port exposed to the gateway.
#[async_trait]
pub trait SessionQuery: Send + Sync {
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryDto>, String>;
}

/// Memory read port exposed to the gateway.
#[async_trait]
pub trait MemoryQuery: Send + Sync {
    async fn list_episodes(
        &self,
        session: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EpisodeDto>, String>;
    async fn memory_graph(&self) -> Result<MemoryGraphDto, String>;
}

/// Memory append port.
#[async_trait]
pub trait MemoryCommand: Send + Sync {
    async fn append_episode(
        &self,
        session: &str,
        role: &str,
        content: &str,
    ) -> Result<EpisodeDto, String>;
}

/// Durable memory governance command port.
#[async_trait]
pub trait MemoryGovernanceCommand: Send + Sync {
    async fn protect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String>;
    async fn unprotect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String>;
    async fn forget_episode(
        &self,
        id: &str,
        expected_rev: u64,
        reason: Option<&str>,
    ) -> Result<EpisodeMutationDto, String>;
}

/// Tool catalog read port.
#[async_trait]
pub trait ToolCatalogQuery: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<ToolDto>, String>;
}

/// Trace query port.
#[async_trait]
pub trait TraceQuery: Send + Sync {
    async fn list_traces(&self, limit: usize) -> Result<Vec<TraceSummaryDto>, String>;
    async fn trace_detail(&self, trace_id: &str) -> Result<Option<TraceDetailDto>, String>;
}

/// Trace archive write port.
#[async_trait]
pub trait TraceCommand: Send + Sync {
    async fn append_trace(&self, trace_id: &str, spans: Vec<TraceSpanDto>);
}

/// Audit query and append ports.
#[async_trait]
pub trait AuditQuery: Send + Sync {
    async fn list_audit(&self, limit: usize) -> Result<Vec<AuditDto>, String>;
}

#[async_trait]
pub trait AuditCommand: Send + Sync {
    async fn append_audit(&self, event: &str, detail: Option<&str>);
}

/// Permission/grant query port.
#[async_trait]
pub trait GrantQuery: Send + Sync {
    async fn list_grants(&self) -> Result<Vec<GrantDto>, String>;
}

/// Permission/grant command port.
#[async_trait]
pub trait GrantCommand: Send + Sync {
    async fn revoke_grant(&self, capability: &str) -> Result<GrantMutationDto, String>;
}

/// Behavior/module projection port.
#[async_trait]
pub trait ModuleQuery: Send + Sync {
    async fn list_modules(&self) -> Result<Vec<OrganDto>, String>;
}

/// Safety guard query port.
#[async_trait]
pub trait SafetyGuardQuery: Send + Sync {
    async fn status(&self) -> Result<GuardStatusDto, String>;
    async fn recent_events(&self, limit: usize) -> Result<Vec<GuardEventDto>, String>;
    async fn dry_run(&self, req: GuardDryRunRequest) -> Result<GuardDryRunResponse, String>;
}

#[async_trait]
impl SafetyGuardQuery for apeireth_guard::BehaviorChainGuardHook {
    async fn status(&self) -> Result<GuardStatusDto, String> {
        Ok(self.status())
    }

    async fn recent_events(&self, limit: usize) -> Result<Vec<GuardEventDto>, String> {
        Ok(self.recent_events(Some(limit)))
    }

    async fn dry_run(&self, req: GuardDryRunRequest) -> Result<GuardDryRunResponse, String> {
        Ok(self.dry_run(&req))
    }
}

/// Workbench turn query port.
#[async_trait]
pub trait WorkbenchQuery: Send + Sync {
    async fn turn_status(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<WorkbenchTurnDto>, String>;
}

/// Gateway service graph. Presence of a port is the capability fact; there is
/// no second `supports_*` feature-bit interface in the production path.
#[derive(Clone, Default)]
pub struct GatewayServices {
    pub sessions: Option<Arc<dyn SessionQuery>>,
    pub memory: Option<Arc<dyn MemoryQuery>>,
    pub memory_commands: Option<Arc<dyn MemoryCommand>>,
    pub memory_governance: Option<Arc<dyn MemoryGovernanceCommand>>,
    pub tools: Option<Arc<dyn ToolCatalogQuery>>,
    pub traces: Option<Arc<dyn TraceQuery>>,
    pub trace_commands: Option<Arc<dyn TraceCommand>>,
    pub audit: Option<Arc<dyn AuditQuery>>,
    pub audit_commands: Option<Arc<dyn AuditCommand>>,
    pub grants: Option<Arc<dyn GrantQuery>>,
    pub grant_commands: Option<Arc<dyn GrantCommand>>,
    pub modules: Option<Arc<dyn ModuleQuery>>,
    pub safety_guard: Option<Arc<dyn SafetyGuardQuery>>,
    pub workbench: Option<Arc<dyn WorkbenchQuery>>,
}

impl GatewayServices {
    /// Compatibility bridge for callers still handing the gateway a legacy
    /// panel object. New production composition should populate ports directly.
    pub fn from_panel(panel: Option<Arc<dyn PanelData>>) -> Self {
        let Some(panel) = panel else {
            return Self::default();
        };
        let adapter = Arc::new(PanelDataAdapter {
            inner: panel.clone(),
        });
        let memory = panel.supports_memory();
        let permissions = panel.supports_permissions();
        let modules = panel.supports_organs();
        Self {
            sessions: Some(adapter.clone()),
            memory: memory.then(|| adapter.clone() as Arc<dyn MemoryQuery>),
            memory_commands: memory.then(|| adapter.clone() as Arc<dyn MemoryCommand>),
            memory_governance: memory.then(|| adapter.clone() as Arc<dyn MemoryGovernanceCommand>),
            tools: Some(adapter.clone()),
            traces: Some(adapter.clone()),
            trace_commands: Some(adapter.clone()),
            audit: Some(adapter.clone()),
            audit_commands: Some(adapter.clone()),
            grants: permissions.then(|| adapter.clone() as Arc<dyn GrantQuery>),
            grant_commands: permissions.then(|| adapter.clone() as Arc<dyn GrantCommand>),
            modules: modules.then(|| adapter as Arc<dyn ModuleQuery>),
            safety_guard: None,
            workbench: None,
        }
    }
}

struct PanelDataAdapter {
    inner: Arc<dyn PanelData>,
}

#[async_trait]
impl SessionQuery for PanelDataAdapter {
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryDto>, String> {
        self.inner.list_sessions().await
    }
}

#[async_trait]
impl MemoryQuery for PanelDataAdapter {
    async fn list_episodes(
        &self,
        session: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EpisodeDto>, String> {
        self.inner.list_episodes(session, query, limit).await
    }

    async fn memory_graph(&self) -> Result<MemoryGraphDto, String> {
        self.inner.memory_graph().await
    }
}

#[async_trait]
impl MemoryCommand for PanelDataAdapter {
    async fn append_episode(
        &self,
        session: &str,
        role: &str,
        content: &str,
    ) -> Result<EpisodeDto, String> {
        self.inner.append_episode(session, role, content).await
    }
}

#[async_trait]
impl MemoryGovernanceCommand for PanelDataAdapter {
    async fn protect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        self.inner.protect_episode(id, expected_rev).await
    }

    async fn unprotect_episode(
        &self,
        id: &str,
        expected_rev: u64,
    ) -> Result<EpisodeMutationDto, String> {
        self.inner.unprotect_episode(id, expected_rev).await
    }

    async fn forget_episode(
        &self,
        id: &str,
        expected_rev: u64,
        reason: Option<&str>,
    ) -> Result<EpisodeMutationDto, String> {
        self.inner.forget_episode(id, expected_rev, reason).await
    }
}

#[async_trait]
impl ToolCatalogQuery for PanelDataAdapter {
    async fn list_tools(&self) -> Result<Vec<ToolDto>, String> {
        self.inner.list_tools().await
    }
}

#[async_trait]
impl TraceQuery for PanelDataAdapter {
    async fn list_traces(&self, limit: usize) -> Result<Vec<TraceSummaryDto>, String> {
        self.inner.list_traces(limit).await
    }

    async fn trace_detail(&self, trace_id: &str) -> Result<Option<TraceDetailDto>, String> {
        self.inner.trace_detail(trace_id).await
    }
}

#[async_trait]
impl TraceCommand for PanelDataAdapter {
    async fn append_trace(&self, trace_id: &str, spans: Vec<TraceSpanDto>) {
        self.inner.append_trace(trace_id, spans).await
    }
}

#[async_trait]
impl AuditQuery for PanelDataAdapter {
    async fn list_audit(&self, limit: usize) -> Result<Vec<AuditDto>, String> {
        self.inner.list_audit(limit).await
    }
}

#[async_trait]
impl AuditCommand for PanelDataAdapter {
    async fn append_audit(&self, event: &str, detail: Option<&str>) {
        self.inner.append_audit(event, detail).await
    }
}

#[async_trait]
impl GrantQuery for PanelDataAdapter {
    async fn list_grants(&self) -> Result<Vec<GrantDto>, String> {
        self.inner.list_grants().await
    }
}

#[async_trait]
impl GrantCommand for PanelDataAdapter {
    async fn revoke_grant(&self, capability: &str) -> Result<GrantMutationDto, String> {
        self.inner.revoke_grant(capability).await
    }
}

#[async_trait]
impl ModuleQuery for PanelDataAdapter {
    async fn list_modules(&self) -> Result<Vec<OrganDto>, String> {
        self.inner.list_organs().await
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Panel routes without attached state — merge into the canonical router
/// before its final `.with_state(...)`.
pub fn panel_routes() -> Router<GatewayState> {
    Router::new()
        .route("/v1/panel/sessions", get(list_sessions))
        .route("/v1/panel/traces", get(list_traces))
        .route("/v1/panel/traces/:trace_id", get(trace_detail))
        .route("/v1/tools/list", get(list_tools))
        .route("/v1/panel/audit", get(list_audit))
        .route("/v1/apeireth/capabilities", get(capabilities))
        .route("/v1/panel/memory/episodes", get(list_episodes))
        .route("/v1/memory/append", axum::routing::post(append_episode))
        .route(
            "/v1/apeireth/memory/episodes/:id/forget",
            axum::routing::post(forget_episode),
        )
        .route(
            "/v1/apeireth/memory/episodes/:id/protect",
            axum::routing::post(protect_episode),
        )
        .route(
            "/v1/apeireth/memory/episodes/:id/unprotect",
            axum::routing::post(unprotect_episode),
        )
        .route("/v1/panel/graph", get(memory_graph))
        .route("/v1/panel/grants", get(list_grants))
        .route("/v1/panel/grants/revoke", axum::routing::post(revoke_grant))
        .route("/v1/organs", get(list_organs))
        .route("/v1/modules", get(list_modules))
        .route("/v1/safety/guard/status", get(guard_status))
        .route("/v1/panel/safety/guard/status", get(guard_status))
        .route("/v1/safety/guard/events", get(guard_events))
        .route("/v1/panel/safety/guard/events", get(guard_events))
        .route(
            "/v1/safety/guard/evaluate",
            axum::routing::post(guard_evaluate),
        )
        .route("/v1/workbench/turn", get(workbench_turn))
        .route("/v1/panel/workbench/turn", get(workbench_turn))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unsupported(what: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": {
                "code": "unsupported",
                "message": format!("{what} 不支持: 当前运行时未实现该内省 API (Apeireth 2.0 canonical gateway)")
            }
        })),
    )
        .into_response()
}

fn panel_error(message: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": { "code": "runtime_error", "message": message } })),
    )
        .into_response()
}

fn limit_of(query: Option<usize>) -> usize {
    query.unwrap_or(50).clamp(1, 500)
}

#[derive(Debug, Default, Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_sessions(State(state): State<GatewayState>) -> Response {
    let Some(sessions) = &state.services.sessions else {
        return unsupported("sessions.read");
    };
    match sessions.list_sessions().await {
        Ok(sessions) => (
            StatusCode::OK,
            Json(serde_json::json!({ "sessions": sessions })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn list_tools(State(state): State<GatewayState>) -> Response {
    let Some(tools) = &state.services.tools else {
        return unsupported("tools.list");
    };
    match tools.list_tools().await {
        Ok(tools) => (StatusCode::OK, Json(serde_json::json!({ "tools": tools }))).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn list_traces(State(state): State<GatewayState>, Query(q): Query<LimitQuery>) -> Response {
    let Some(traces) = &state.services.traces else {
        return unsupported("trace.read");
    };
    match traces.list_traces(limit_of(q.limit)).await {
        Ok(traces) => (
            StatusCode::OK,
            Json(serde_json::json!({ "traces": traces })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn trace_detail(State(state): State<GatewayState>, Path(trace_id): Path<String>) -> Response {
    let Some(traces) = &state.services.traces else {
        return unsupported("trace.read");
    };
    match traces.trace_detail(&trace_id).await {
        Ok(Some(detail)) => (StatusCode::OK, Json(detail)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": { "code": "not_found", "message": format!("trace {trace_id} not found") } })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn list_audit(State(state): State<GatewayState>, Query(q): Query<LimitQuery>) -> Response {
    let Some(audit) = &state.services.audit else {
        return unsupported("audit.read");
    };
    match audit.list_audit(limit_of(q.limit)).await {
        Ok(events) => (
            StatusCode::OK,
            Json(serde_json::json!({ "events": events })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn guard_status(State(state): State<GatewayState>) -> Response {
    let Some(guard) = &state.services.safety_guard else {
        return unsupported("safety.guard.status.read");
    };
    match guard.status().await {
        Ok(status) => (StatusCode::OK, Json(status)).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn guard_events(State(state): State<GatewayState>, Query(q): Query<LimitQuery>) -> Response {
    let Some(guard) = &state.services.safety_guard else {
        return unsupported("safety.guard.events.read");
    };
    match guard.recent_events(limit_of(q.limit)).await {
        Ok(events) => (
            StatusCode::OK,
            Json(serde_json::json!({ "events": events })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn guard_evaluate(
    State(state): State<GatewayState>,
    Json(request): Json<GuardDryRunRequest>,
) -> Response {
    let Some(guard) = &state.services.safety_guard else {
        return unsupported("safety.guard.evaluate");
    };
    match guard.dry_run(request).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(e) => panel_error(e),
    }
}

#[derive(Debug, Default, Deserialize)]
struct WorkbenchQueryParam {
    session: Option<String>,
}

async fn workbench_turn(
    State(state): State<GatewayState>,
    Query(q): Query<WorkbenchQueryParam>,
) -> Response {
    let Some(workbench) = &state.services.workbench else {
        return unsupported("workbench.turn.read");
    };
    match workbench.turn_status(q.session.as_deref()).await {
        Ok(turn) => (StatusCode::OK, Json(serde_json::json!({ "turn": turn }))).into_response(),
        Err(e) => panel_error(e),
    }
}

// ---------------------------------------------------------------------------
// Memory introspection
// ---------------------------------------------------------------------------

fn memory_unavailable(state: &GatewayState) -> Option<Response> {
    if state.services.memory.is_none() {
        return Some(unsupported("memory.read"));
    }
    None
}

fn memory_write_unavailable(state: &GatewayState) -> Option<Response> {
    if state.services.memory_commands.is_none() {
        return Some(unsupported("memory.write"));
    }
    None
}

fn memory_governance_unavailable(state: &GatewayState) -> Option<Response> {
    if state.services.memory_governance.is_none() {
        return Some(unsupported("memory.governance"));
    }
    None
}

#[derive(Debug, Default, Deserialize)]
struct EpisodeQuery {
    limit: Option<usize>,
    q: Option<String>,
    session: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EpisodeAppendRequest {
    session: String,
    content: String,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EpisodeMutationRequest {
    expected_rev: u64,
    #[serde(default)]
    reason: Option<String>,
}

async fn list_episodes(
    State(state): State<GatewayState>,
    Query(q): Query<EpisodeQuery>,
) -> Response {
    if let Some(response) = memory_unavailable(&state) {
        return response;
    }
    let memory = state.services.memory.as_ref().expect("checked above");
    match memory
        .list_episodes(q.session.as_deref(), q.q.as_deref(), limit_of(q.limit))
        .await
    {
        Ok(episodes) => (
            StatusCode::OK,
            Json(serde_json::json!({ "episodes": episodes })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn append_episode(
    State(state): State<GatewayState>,
    Json(request): Json<EpisodeAppendRequest>,
) -> Response {
    if let Some(response) = memory_write_unavailable(&state) {
        return response;
    }
    if request.content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": { "code": "invalid_request", "message": "content must not be empty" } })),
        )
            .into_response();
    }
    let memory = state
        .services
        .memory_commands
        .as_ref()
        .expect("checked above");
    let role = request.role.as_deref().unwrap_or("user");
    match memory
        .append_episode(&request.session, role, &request.content)
        .await
    {
        Ok(episode) => (StatusCode::CREATED, Json(episode)).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn protect_episode(
    State(state): State<GatewayState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<EpisodeMutationRequest>,
) -> Response {
    if let Some(response) = memory_governance_unavailable(&state) {
        return response;
    }
    let memory = state
        .services
        .memory_governance
        .as_ref()
        .expect("checked above");
    match memory.protect_episode(&id, request.expected_rev).await {
        Ok(mutation) => (StatusCode::OK, Json(mutation)).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn unprotect_episode(
    State(state): State<GatewayState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<EpisodeMutationRequest>,
) -> Response {
    if let Some(response) = memory_governance_unavailable(&state) {
        return response;
    }
    let memory = state
        .services
        .memory_governance
        .as_ref()
        .expect("checked above");
    match memory.unprotect_episode(&id, request.expected_rev).await {
        Ok(mutation) => (StatusCode::OK, Json(mutation)).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn forget_episode(
    State(state): State<GatewayState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<EpisodeMutationRequest>,
) -> Response {
    if let Some(response) = memory_governance_unavailable(&state) {
        return response;
    }
    let memory = state
        .services
        .memory_governance
        .as_ref()
        .expect("checked above");
    match memory
        .forget_episode(&id, request.expected_rev, request.reason.as_deref())
        .await
    {
        Ok(mutation) => (StatusCode::OK, Json(mutation)).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn memory_graph(State(state): State<GatewayState>) -> Response {
    if let Some(response) = memory_unavailable(&state) {
        return response;
    }
    let memory = state.services.memory.as_ref().expect("checked above");
    match memory.memory_graph().await {
        Ok(graph) => (StatusCode::OK, Json(graph)).into_response(),
        Err(e) => panel_error(e),
    }
}

// ---------------------------------------------------------------------------
// Permissions (grants / hot revoke) and organs
// ---------------------------------------------------------------------------

fn permissions_unavailable(state: &GatewayState) -> Option<Response> {
    if state.services.grants.is_none() {
        return Some(unsupported("permissions.grants.read"));
    }
    None
}

#[derive(Debug, Deserialize)]
struct RevokeGrantRequest {
    capability: String,
}

async fn list_grants(State(state): State<GatewayState>) -> Response {
    if let Some(response) = permissions_unavailable(&state) {
        return response;
    }
    let grants = state.services.grants.as_ref().expect("checked above");
    match grants.list_grants().await {
        Ok(grants) => (
            StatusCode::OK,
            Json(serde_json::json!({ "grants": grants })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn revoke_grant(
    State(state): State<GatewayState>,
    Json(request): Json<RevokeGrantRequest>,
) -> Response {
    if let Some(response) = permissions_unavailable(&state) {
        return response;
    }
    let Some(commands) = &state.services.grant_commands else {
        return unsupported("permissions.revoke");
    };
    match commands.revoke_grant(&request.capability).await {
        Ok(mutation) => (StatusCode::OK, Json(mutation)).into_response(),
        Err(e) => panel_error(e),
    }
}

async fn list_organs(State(state): State<GatewayState>) -> Response {
    let Some(modules) = &state.services.modules else {
        return unsupported("organs.list");
    };
    match modules.list_modules().await {
        Ok(organs) => (
            StatusCode::OK,
            Json(serde_json::json!({ "organs": organs })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

async fn list_modules(State(state): State<GatewayState>) -> Response {
    let Some(modules) = &state.services.modules else {
        return unsupported("modules.list");
    };
    match modules.list_modules().await {
        Ok(modules) => (
            StatusCode::OK,
            Json(serde_json::json!({ "modules": modules })),
        )
            .into_response(),
        Err(e) => panel_error(e),
    }
}

// ---------------------------------------------------------------------------
// Capability manifest
// ---------------------------------------------------------------------------

fn cap(
    id: &str,
    read: bool,
    write: bool,
    ops: &[&str],
    supported: bool,
    available: bool,
) -> serde_json::Value {
    cap_with_reason(id, read, write, ops, supported, available, None)
}

fn cap_with_reason(
    id: &str,
    read: bool,
    write: bool,
    ops: &[&str],
    supported: bool,
    available: bool,
    reason: Option<&str>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": id,
        "supported": supported,
        "read": read,
        "write": write,
        "version": 1,
        "operations": ops,
        "available": available,
    });
    if !available {
        value["reason"] = serde_json::json!(reason.unwrap_or(if supported {
            "provider_not_configured"
        } else {
            "platform_unsupported"
        }));
    }
    value
}

fn cap_alias(
    id: &str,
    alias_of: &str,
    read: bool,
    write: bool,
    ops: &[&str],
    supported: bool,
    available: bool,
) -> serde_json::Value {
    let mut value = cap(id, read, write, ops, supported, available);
    value["alias_of"] = serde_json::json!(alias_of);
    value
}

async fn capabilities(State(state): State<GatewayState>) -> Response {
    let services = &state.services;
    let sessions_supported = services.sessions.is_some();
    let memory_supported = services.memory.is_some();
    let memory_write_supported = services.memory_commands.is_some();
    let memory_governance_supported = services.memory_governance.is_some();
    let tools_supported = services.tools.is_some();
    let trace_supported = services.traces.is_some();
    let audit_supported = services.audit.is_some();
    let permissions_supported = services.grants.is_some();
    let permissions_write_supported = services.grant_commands.is_some();
    let organs_supported = services.modules.is_some();
    let safety_supported = services.safety_guard.is_some();
    let workbench_supported = services.workbench.is_some();

    let memory_ids = [
        ("memory.read", true, false, &["list", "read"] as &[&str]),
        ("memory.write", false, true, &["append"] as &[&str]),
        ("memory.update", false, true, &["update"] as &[&str]),
        ("memory.forget", false, true, &["forget"] as &[&str]),
        ("memory.protect", false, true, &["protect"] as &[&str]),
        ("memory.unprotect", false, true, &["unprotect"] as &[&str]),
    ];
    let memory = memory_ids
        .iter()
        .map(|(id, read, write, operations)| {
            let (supported, available) = match *id {
                "memory.read" => (memory_supported, memory_supported),
                "memory.write" => (memory_write_supported, memory_write_supported),
                "memory.update" => (false, false),
                _ => (memory_governance_supported, memory_governance_supported),
            };
            if *id == "memory.update" {
                cap_with_reason(
                    id,
                    *read,
                    *write,
                    operations,
                    supported,
                    available,
                    Some("not_implemented"),
                )
            } else {
                cap(id, *read, *write, operations, supported, available)
            }
        })
        .chain(std::iter::once(cap(
            "memory.graph.read",
            true,
            false,
            &["graph"],
            memory_supported,
            memory_supported,
        )))
        .collect::<Vec<_>>();

    let manifest = serde_json::json!({
        "schema_version": 1,
        "runtime": { "service": "apeireth-gateway-2.0", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": [
            { "name": "health", "capabilities": [ cap("health", true, false, &["check"], true, true) ] },
            { "name": "models", "capabilities": [ cap("models.list", true, false, &["list"], true, !state.runtime.providers().is_empty()) ] },
            { "name": "providers", "capabilities": [ cap("providers.list", true, false, &["list"], true, !state.runtime.providers().is_empty()) ] },
            { "name": "runtime", "capabilities": [ cap("runtime.snapshot.read", true, false, &["read"], true, true) ] },
            { "name": "chat", "capabilities": [ cap("chat.completions", true, true, &["complete", "stream"], true, !state.runtime.providers().is_empty()) ] },
            { "name": "sessions", "capabilities": [ cap("sessions.read", true, false, &["list"], sessions_supported, sessions_supported) ] },
            { "name": "memory", "capabilities": memory },
            { "name": "tools", "capabilities": [ cap("tools.list", true, false, &["list"], tools_supported, tools_supported) ] },
            { "name": "permissions", "capabilities": [
                cap("permissions.approval.read", true, false, &["list"], true, true),
                cap("permissions.approval.resolve", false, true, &["resolve"], true, true),
                cap_alias("approvals.read", "permissions.approval.read", true, false, &["list"], true, true),
                cap_alias("approvals.resolve", "permissions.approval.resolve", false, true, &["resolve"], true, true),
                cap("permissions.grants.read", true, false, &["list"], permissions_supported, permissions_supported),
                cap("permissions.revoke", false, true, &["revoke"], permissions_write_supported, permissions_write_supported),
            ] },
            { "name": "organs", "capabilities": [
                cap("organs.list", true, false, &["list"], organs_supported, organs_supported),
                cap("modules.list", true, false, &["list"], organs_supported, organs_supported),
            ] },
            { "name": "safety", "capabilities": [
                cap("safety.guard.status.read", true, false, &["read"], safety_supported, safety_supported),
                cap("safety.guard.events.read", true, false, &["list"], safety_supported, safety_supported),
                cap("safety.guard.evaluate", false, true, &["evaluate"], safety_supported, safety_supported),
            ] },
            { "name": "workbench", "capabilities": [
                cap("workbench.turn.read", true, false, &["read"], workbench_supported, workbench_supported),
            ] },
            { "name": "voice", "capabilities": [
                cap_with_reason("voice.duplex", false, false, &["duplex", "stream"], false, false, Some("not_assembled")),
            ] },
            { "name": "subagents", "capabilities": [
                cap_with_reason("subagents.orchestration", false, false, &["spawn", "coordinate"], false, false, Some("not_assembled")),
            ] },
            { "name": "trace", "capabilities": [ cap("trace.read", true, false, &["list", "detail"], trace_supported, trace_supported) ] },
            { "name": "audit", "capabilities": [ cap("audit.read", true, false, &["list"], audit_supported, audit_supported) ] },
            { "name": "activity", "capabilities": [ cap("activity.sse", true, false, &["subscribe"], true, true) ] },
        ]
    });
    (StatusCode::OK, Json(manifest)).into_response()
}
