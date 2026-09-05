//! HTTP and native chat adapters for the canonical runtime.
//!
//! This module owns decoding, transport validation, canonical turn
//! construction, runtime invocation, and response encoding. It intentionally
//! owns no provider selection, governance composition, session orchestration,
//! plugin lifecycle, tool dispatch, retry, or agent loop.

use std::sync::Arc;

use apeireth_core::kernel::{ApprovalId, SessionId, Timestamp};
use apeireth_protocol::canonical::{ContentPart, NormalizedUsage};
use apeireth_runtime::canonical::{
    ApprovalDecision, ApprovalResolution, ExecutionTrace, PendingApprovalView, Runtime,
    RuntimeError, TraceEvent, TurnOutcome, TurnRequest,
};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::events::{events_handler, EventBus, GatewayEvent};
use crate::panels::{panel_routes, GatewayServices, GatewayState, PanelData};

/// Native gateway request. HTTP and CLI transports can both construct it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CanonicalChatRequest {
    /// Existing canonical session, or a fresh session when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// User input for this turn.
    pub input: String,
    /// Optional model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// System instruction used only when the session is new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}

impl CanonicalChatRequest {
    /// A request containing one user turn.
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            session: None,
            input: input.into(),
            model: None,
            system: None,
        }
    }
}

/// Transport-neutral response returned after canonical execution.
#[derive(Debug, Clone, Serialize)]
pub struct CanonicalChatResponse {
    /// Stable session used by the full turn.
    pub session: SessionId,
    /// Runtime request identifier.
    pub request: String,
    /// Runtime trace identifier.
    pub trace_id: String,
    /// Final assistant text.
    pub text: String,
    /// Provider capability that served the final round.
    pub served_by: String,
    /// Provider round-trips taken.
    pub rounds: u32,
    /// Canonical token accounting.
    pub usage: NormalizedUsage,
    /// Structured execution metadata; never raw model reasoning.
    pub trace: ExecutionTrace,
    /// Product-facing execution events derived from the trace.
    ///
    /// Desktop may render these. It must not execute tools from them.
    pub events: Vec<CanonicalExecutionEvent>,
}

/// Minimal product-facing execution event.
///
/// These are observations of the Main Loop. They are not a tool-call protocol
/// and they never authorize the client to run a capability.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CanonicalExecutionEvent {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub succeeded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
}

fn events_from_trace(trace: &ExecutionTrace) -> Vec<CanonicalExecutionEvent> {
    let mut events = Vec::new();
    for entry in &trace.entries {
        match &entry.event {
            TraceEvent::CapabilityDispatched {
                capability,
                tool_call_id,
                round,
            } => events.push(CanonicalExecutionEvent {
                event: "tool_started".into(),
                tool_name: Some(capability.to_string()),
                capability_id: Some(capability.to_string()),
                tool_call_id: Some(tool_call_id.clone()),
                succeeded: None,
                approval_id: None,
                round: Some(*round),
            }),
            TraceEvent::CapabilityCompleted {
                capability,
                tool_call_id,
                succeeded,
                round,
            } => events.push(CanonicalExecutionEvent {
                event: if *succeeded {
                    "tool_completed".into()
                } else {
                    "tool_failed".into()
                },
                tool_name: Some(capability.to_string()),
                capability_id: Some(capability.to_string()),
                tool_call_id: Some(tool_call_id.clone()),
                succeeded: Some(*succeeded),
                approval_id: None,
                round: Some(*round),
            }),
            TraceEvent::ApprovalRequested {
                approval_id,
                capability,
                tool_call_id,
                round,
            } => events.push(CanonicalExecutionEvent {
                event: "approval_required".into(),
                tool_name: Some(capability.to_string()),
                capability_id: Some(capability.to_string()),
                tool_call_id: Some(tool_call_id.clone()),
                succeeded: None,
                approval_id: Some(approval_id.to_string()),
                round: Some(*round),
            }),
            _ => {}
        }
    }
    events
}

/// Transport-neutral pending-approval payload. Exposes identity and safe
/// metadata only; it never includes the executable frozen payload.
#[derive(Debug, Clone, Serialize)]
pub struct CanonicalPendingApproval {
    pub session: SessionId,
    pub approval_id: ApprovalId,
    pub request: String,
    pub trace_id: String,
    pub capability_id: String,
    pub tool_name: String,
    pub governance_hook: String,
    pub governance_reason: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

impl From<PendingApprovalView> for CanonicalPendingApproval {
    fn from(view: PendingApprovalView) -> Self {
        Self {
            session: view.session_id,
            approval_id: view.approval_id,
            request: view.request_id.to_string(),
            trace_id: view.trace_id.to_string(),
            capability_id: view.capability_id.to_string(),
            tool_name: view.tool_name,
            governance_hook: view.governance_hook,
            governance_reason: view.governance_reason,
            created_at: view.created_at,
            expires_at: view.expires_at,
        }
    }
}

/// Result of a canonical chat or approval-resume call.
#[derive(Debug, Clone)]
pub enum CanonicalChatOutcome {
    /// The turn completed.
    Completed(CanonicalChatResponse),
    /// The turn is paused for human approval. The `ApprovalId` is retained.
    PendingApproval(CanonicalPendingApproval),
}

/// Failure at the gateway adapter boundary.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalEntryError {
    /// Transport input was not meaningful enough to form a turn.
    #[error("invalid chat request: {0}")]
    InvalidRequest(String),
    /// Canonical runtime execution failed.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// Request to resolve one pending approval.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CanonicalApprovalRequest {
    pub session: SessionId,
    pub approval: ApprovalId,
    /// `approve`, `reject`, or `cancel`.
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Invoke the canonical runtime through the real gateway entry adapter.
pub async fn execute_chat(
    runtime: &Runtime,
    request: CanonicalChatRequest,
) -> Result<CanonicalChatOutcome, CanonicalEntryError> {
    if request.input.trim().is_empty() {
        return Err(CanonicalEntryError::InvalidRequest(
            "input must not be empty".into(),
        ));
    }

    let session = request.session.unwrap_or_else(SessionId::new);
    let mut turn = TurnRequest::new(session, request.input);
    if let Some(model) = request.model {
        turn = turn.with_model(model);
    }
    if let Some(system) = request.system {
        turn = turn.with_system(system);
    }

    Ok(turn_outcome_to_chat(runtime.execute_outcome(turn).await?))
}

/// Resolve a pending approval through the canonical runtime API.
pub async fn resolve_approval(
    runtime: &Runtime,
    request: CanonicalApprovalRequest,
) -> Result<CanonicalChatOutcome, CanonicalEntryError> {
    let decision = parse_approval_decision(&request.decision, request.reason.clone())?;
    match runtime
        .resolve_approval(request.session, request.approval, decision)
        .await?
    {
        ApprovalResolution::Resumed(outcome) => Ok(turn_outcome_to_chat(outcome)),
        ApprovalResolution::AlreadyResolved { status } => Err(CanonicalEntryError::InvalidRequest(
            format!("approval already resolved: {status:?}"),
        )),
        ApprovalResolution::ExecutionInterrupted { approval_id } => {
            Err(CanonicalEntryError::InvalidRequest(format!(
                "approval {approval_id} was interrupted and must not be retried automatically"
            )))
        }
        ApprovalResolution::Expired => Err(CanonicalEntryError::InvalidRequest(
            "approval expired before it was resolved".into(),
        )),
        ApprovalResolution::NotFound => Err(CanonicalEntryError::InvalidRequest(
            "approval was not found for this session".into(),
        )),
    }
}

fn turn_outcome_to_chat(outcome: TurnOutcome) -> CanonicalChatOutcome {
    match outcome {
        TurnOutcome::Completed(response) => {
            CanonicalChatOutcome::Completed(CanonicalChatResponse {
                session: response.session,
                request: response.request.to_string(),
                trace_id: response.trace.trace.to_string(),
                text: response.text,
                served_by: response.served_by.to_string(),
                rounds: response.rounds,
                usage: response.usage,
                trace: response.trace.clone(),
                events: events_from_trace(&response.trace),
            })
        }
        TurnOutcome::PendingApproval(view) => {
            CanonicalChatOutcome::PendingApproval(CanonicalPendingApproval::from(view))
        }
    }
}

fn parse_approval_decision(
    decision: &str,
    reason: Option<String>,
) -> Result<ApprovalDecision, CanonicalEntryError> {
    match decision.trim().to_ascii_lowercase().as_str() {
        "approve" => Ok(ApprovalDecision::Approve),
        "reject" => Ok(ApprovalDecision::Reject { reason }),
        "cancel" => Ok(ApprovalDecision::Cancel { reason }),
        other => Err(CanonicalEntryError::InvalidRequest(format!(
            "unknown approval decision {other:?}; expected approve, reject, or cancel"
        ))),
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatRequest {
    model: Option<String>,
    messages: Vec<OpenAiMessage>,
    #[serde(default)]
    session_id: Option<SessionId>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Serialize)]
struct OpenAiChatResponse {
    id: String,
    object: &'static str,
    created: i64,
    model: String,
    choices: Vec<OpenAiChoice>,
    usage: NormalizedUsage,
    apeireth: OpenAiExecutionMetadata,
}

#[derive(Debug, Serialize)]
struct OpenAiChoice {
    index: u32,
    message: OpenAiAssistantMessage,
    finish_reason: &'static str,
}

#[derive(Debug, Serialize)]
struct OpenAiAssistantMessage {
    role: &'static str,
    content: String,
}

#[derive(Debug, Serialize)]
struct OpenAiExecutionMetadata {
    session_id: String,
    trace_id: String,
    served_by: String,
    rounds: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    events: Vec<CanonicalExecutionEvent>,
}

#[derive(Debug, Serialize)]
struct OpenAiStreamChunk {
    id: String,
    object: &'static str,
    created: i64,
    model: String,
    choices: Vec<OpenAiStreamChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apeireth: Option<OpenAiExecutionMetadata>,
}

#[derive(Debug, Serialize)]
struct OpenAiStreamChoice {
    index: u32,
    delta: OpenAiStreamDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct OpenAiStreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct ModelListItem {
    id: String,
    object: &'static str,
    created: i64,
    owned_by: String,
}

#[derive(Debug, Serialize)]
struct ModelListResponse {
    object: &'static str,
    data: Vec<ModelListItem>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

type HttpError = (StatusCode, Json<ErrorBody>);

/// Assemble the shared gateway state (runtime + optional panels + event bus).
pub fn build_gateway_state(
    runtime: Arc<Runtime>,
    panels: Option<Arc<dyn PanelData>>,
) -> GatewayState {
    build_gateway_state_with_services(runtime, GatewayServices::from_panel(panels))
}

/// Assemble gateway state from bounded-context ports supplied by the
/// composition root. This is the production path; the legacy `PanelData`
/// adapter above exists only for compatibility with older embedders/tests.
pub fn build_gateway_state_with_services(
    runtime: Arc<Runtime>,
    services: GatewayServices,
) -> GatewayState {
    let events = EventBus::default();
    let observations = Arc::new(crate::events::RuntimeObservationSink::new(
        services.trace_commands.clone(),
        services.audit_commands.clone(),
    ));
    runtime.add_event_sink(Arc::new(
        apeireth_runtime::canonical::CompositeRuntimeEventSink::new(vec![
            Arc::new(events.clone()),
            observations.clone(),
        ]),
    ));
    GatewayState {
        runtime,
        services,
        events,
        observations,
    }
}

/// Build the production HTTP router around one long-lived canonical runtime.
///
/// Panel routes answer `501 unsupported` while no [`PanelData`] is attached —
/// see [`canonical_router_with_panels`].
pub fn canonical_router(runtime: Arc<Runtime>) -> Router {
    canonical_router_with_services(runtime, GatewayServices::default())
}

/// Build the production router with optional panel/introspection backends.
pub fn canonical_router_with_panels(
    runtime: Arc<Runtime>,
    panels: Option<Arc<dyn PanelData>>,
) -> Router {
    canonical_router_with_state(build_gateway_state(runtime, panels))
}

/// Build the production router over explicit bounded-context gateway ports.
pub fn canonical_router_with_services(runtime: Arc<Runtime>, services: GatewayServices) -> Router {
    canonical_router_with_state(build_gateway_state_with_services(runtime, services))
}

/// Build the production router over an explicit [`GatewayState`] (lets tests
/// keep a handle on the event bus).
pub fn canonical_router_with_state(state: GatewayState) -> Router {
    Router::<GatewayState>::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/providers", get(list_providers))
        .route("/v1/runtime/snapshot", get(runtime_snapshot))
        .route("/v1/apeireth/runtime/snapshot", get(runtime_snapshot))
        .route("/v1/chat", post(native_chat))
        .route("/v1/chat/completions", post(openai_chat))
        .route("/v1/approvals", get(list_pending_approvals))
        .route("/v1/approvals/resolve", post(native_resolve_approval))
        .route("/v1/apeireth/events", get(events_handler))
        .merge(panel_routes())
        // The desktop gateway is loopback by default and does not need an
        // open cross-origin policy. Deployments that intentionally expose the
        // gateway must add an explicit, trusted-origin policy at their edge.
        .with_state(state)
}

/// Serve the canonical gateway until the listener closes.
pub async fn serve_canonical(
    listener: tokio::net::TcpListener,
    runtime: Arc<Runtime>,
    panels: Option<Arc<dyn PanelData>>,
) -> std::io::Result<()> {
    serve_canonical_with_services(listener, runtime, GatewayServices::from_panel(panels)).await
}

/// Serve the canonical gateway over explicit bounded-context gateway ports.
pub async fn serve_canonical_with_services(
    listener: tokio::net::TcpListener,
    runtime: Arc<Runtime>,
    services: GatewayServices,
) -> std::io::Result<()> {
    let state = build_gateway_state_with_services(runtime, services);
    let endpoint = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    state.events.publish(GatewayEvent::new(
        "backend_ready",
        serde_json::json!({ "service": "apeireth-gateway-2.0", "endpoint": endpoint }),
    ));
    axum::serve(listener, canonical_router_with_state(state)).await
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "execution_owner": "apeireth-runtime::canonical"
    }))
}

async fn list_models(State(state): State<GatewayState>) -> Json<ModelListResponse> {
    let created = Timestamp::from_clock(state.runtime.clock().as_ref()).epoch_millis() / 1_000;
    let data = state
        .runtime
        .providers()
        .model_descriptors()
        .into_iter()
        .map(|model| ModelListItem {
            id: model.id.to_string(),
            object: "model",
            created,
            owned_by: model.provider.to_string(),
        })
        .collect();
    Json(ModelListResponse {
        object: "list",
        data,
    })
}

async fn list_providers(State(state): State<GatewayState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "object": "list",
        "data": state.runtime.snapshot().providers,
    }))
}

async fn runtime_snapshot(State(state): State<GatewayState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.runtime.snapshot()))
}

async fn native_chat(
    State(state): State<GatewayState>,
    Json(request): Json<CanonicalChatRequest>,
) -> Result<Response, HttpError> {
    let mut request = request;
    let session = request.session.unwrap_or_else(SessionId::new);
    request.session = Some(session);
    let outcome = execute_chat(state.runtime.as_ref(), request).await;
    state.observations.flush().await;
    let outcome = outcome.map_err(|error| http_error(error, Some(session)))?;
    publish_turn_delta(&state.events, &outcome);
    Ok(chat_http_response(outcome))
}

/// Emit the final assistant text as a transport delta. Lifecycle semantics are
/// emitted by the RuntimeEventSink; this helper does not infer them.
fn publish_turn_delta(bus: &EventBus, outcome: &CanonicalChatOutcome) {
    if let CanonicalChatOutcome::Completed(response) = outcome {
        // v1 honesty: the runtime completes a turn before the gateway can
        // encode it, so `turn_delta` carries the final text as ONE delta.
        bus.publish(GatewayEvent::new(
            "turn_delta",
            serde_json::json!({ "session": response.session, "text": response.text }),
        ));
    }
}

async fn native_resolve_approval(
    State(state): State<GatewayState>,
    Json(request): Json<CanonicalApprovalRequest>,
) -> Result<Response, HttpError> {
    let session = request.session;
    let outcome = resolve_approval(state.runtime.as_ref(), request).await;
    state.observations.flush().await;
    let outcome = outcome.map_err(|error| http_error(error, Some(session)))?;
    publish_turn_delta(&state.events, &outcome);
    Ok(chat_http_response(outcome))
}

#[derive(Debug, Deserialize)]
struct ApprovalInboxQuery {
    session: SessionId,
}

#[derive(Debug, Serialize)]
struct ApprovalInboxResponse {
    session: SessionId,
    approvals: Vec<CanonicalPendingApproval>,
}

async fn list_pending_approvals(
    State(state): State<GatewayState>,
    Query(query): Query<ApprovalInboxQuery>,
) -> Result<Json<ApprovalInboxResponse>, HttpError> {
    let approvals = state
        .runtime
        .pending_approvals(query.session)
        .await
        .map_err(|error| http_error(CanonicalEntryError::Runtime(error), Some(query.session)))?
        .into_iter()
        .map(CanonicalPendingApproval::from)
        .collect();
    Ok(Json(ApprovalInboxResponse {
        session: query.session,
        approvals,
    }))
}

fn chat_http_response(outcome: CanonicalChatOutcome) -> Response {
    match outcome {
        CanonicalChatOutcome::Completed(response) => {
            (StatusCode::OK, Json(response)).into_response()
        }
        CanonicalChatOutcome::PendingApproval(pending) => {
            (StatusCode::ACCEPTED, Json(pending)).into_response()
        }
    }
}

async fn openai_chat(
    State(state): State<GatewayState>,
    Json(request): Json<OpenAiChatRequest>,
) -> Result<Response, HttpError> {
    let is_stream = request.stream;
    let session = request.session_id.unwrap_or_else(SessionId::new);
    let input = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .map(|message| ContentPart::join_text(&ContentPart::from_legacy_value(&message.content)))
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            http_error(
                CanonicalEntryError::InvalidRequest(
                    "messages must contain a non-empty user message".into(),
                ),
                Some(session),
            )
        })?;
    let system = request
        .messages
        .iter()
        .filter(|message| message.role == "system")
        .map(|message| ContentPart::join_text(&ContentPart::from_legacy_value(&message.content)))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let native = CanonicalChatRequest {
        session: Some(session),
        input,
        model: request.model.clone(),
        system: (!system.is_empty()).then_some(system),
    };
    let outcome = execute_chat(state.runtime.as_ref(), native).await;
    state.observations.flush().await;
    let outcome = outcome.map_err(|error| http_error(error, Some(session)))?;
    let outcome = match outcome {
        CanonicalChatOutcome::Completed(completed) => completed,
        CanonicalChatOutcome::PendingApproval(pending) => {
            return Ok((StatusCode::ACCEPTED, Json(pending)).into_response());
        }
    };
    let created = Timestamp::from_clock(state.runtime.clock().as_ref()).epoch_millis() / 1_000;
    let model_name = request.model.unwrap_or_default();
    let events = outcome.events.clone();
    publish_turn_delta(
        &state.events,
        &CanonicalChatOutcome::Completed(outcome.clone()),
    );

    if is_stream {
        // 构建 OpenAI 规范 SSE 流式数据帧
        let chunk_init = OpenAiStreamChunk {
            id: outcome.request.clone(),
            object: "chat.completion.chunk",
            created,
            model: model_name.clone(),
            choices: vec![OpenAiStreamChoice {
                index: 0,
                delta: OpenAiStreamDelta {
                    role: Some("assistant"),
                    content: None,
                },
                finish_reason: None,
            }],
            apeireth: None,
        };
        let chunk_content = OpenAiStreamChunk {
            id: outcome.request.clone(),
            object: "chat.completion.chunk",
            created,
            model: model_name.clone(),
            choices: vec![OpenAiStreamChoice {
                index: 0,
                delta: OpenAiStreamDelta {
                    role: None,
                    content: Some(outcome.text),
                },
                finish_reason: None,
            }],
            apeireth: Some(OpenAiExecutionMetadata {
                session_id: outcome.session.to_string(),
                trace_id: outcome.trace_id.clone(),
                served_by: outcome.served_by.clone(),
                rounds: outcome.rounds,
                events: events.clone(),
            }),
        };
        let chunk_final = OpenAiStreamChunk {
            id: outcome.request,
            object: "chat.completion.chunk",
            created,
            model: model_name,
            choices: vec![OpenAiStreamChoice {
                index: 0,
                delta: OpenAiStreamDelta {
                    role: None,
                    content: None,
                },
                finish_reason: Some("stop"),
            }],
            apeireth: None,
        };

        let body_str = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::to_string(&chunk_init).map_err(|e| http_error(
                CanonicalEntryError::InvalidRequest(e.to_string()),
                Some(session)
            ))?,
            serde_json::to_string(&chunk_content).map_err(|e| http_error(
                CanonicalEntryError::InvalidRequest(e.to_string()),
                Some(session)
            ))?,
            serde_json::to_string(&chunk_final).map_err(|e| http_error(
                CanonicalEntryError::InvalidRequest(e.to_string()),
                Some(session)
            ))?,
        );

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .body(axum::body::Body::from(body_str))
            .map_err(|e| {
                http_error(
                    CanonicalEntryError::InvalidRequest(e.to_string()),
                    Some(session),
                )
            })?)
    } else {
        Ok(Json(OpenAiChatResponse {
            id: outcome.request.clone(),
            object: "chat.completion",
            created,
            model: model_name,
            choices: vec![OpenAiChoice {
                index: 0,
                message: OpenAiAssistantMessage {
                    role: "assistant",
                    content: outcome.text,
                },
                finish_reason: "stop",
            }],
            usage: outcome.usage,
            apeireth: OpenAiExecutionMetadata {
                session_id: outcome.session.to_string(),
                trace_id: outcome.trace_id,
                served_by: outcome.served_by,
                rounds: outcome.rounds,
                events,
            },
        })
        .into_response())
    }
}

fn http_error(error: CanonicalEntryError, session: Option<SessionId>) -> HttpError {
    let status = match &error {
        CanonicalEntryError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        CanonicalEntryError::Runtime(RuntimeError::Denied { .. }) => StatusCode::FORBIDDEN,
        CanonicalEntryError::Runtime(RuntimeError::ApprovalRequired { .. })
        | CanonicalEntryError::Runtime(RuntimeError::SessionApprovalPending { .. }) => {
            StatusCode::CONFLICT
        }
        CanonicalEntryError::Runtime(RuntimeError::NoProvider { .. })
        | CanonicalEntryError::Runtime(RuntimeError::NoHealthyProvider { .. })
        | CanonicalEntryError::Runtime(RuntimeError::Misconfigured(_)) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        CanonicalEntryError::Runtime(RuntimeError::Provider(_))
        | CanonicalEntryError::Runtime(RuntimeError::ProvidersExhausted { .. }) => {
            StatusCode::BAD_GATEWAY
        }
        CanonicalEntryError::Runtime(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorBody {
            error: error.to_string(),
            session_id: session.map(|id| id.to_string()),
        }),
    )
}
