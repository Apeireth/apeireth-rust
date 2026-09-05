//! Gateway-level SSE event bus (`GET /v1/apeireth/events`).
//!
//! Emits product-facing lifecycle events:
//! `backend_ready` / `turn_started` / `turn_delta` / `turn_completed` /
//! `approval_required` / `approval_resolved`.
//!
//! v1 honesty notes (contract §8):
//! - `turn_delta` carries the final assistant text as ONE delta: the canonical
//!   runtime completes a turn before the gateway encodes it, so token-level
//!   deltas are not observable at this boundary.
//! - The bus is in-process broadcast; a subscriber that lags behind is
//!   disconnected by tokio broadcast semantics (no unbounded buffering).

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use apeireth_runtime::canonical::{RuntimeEvent, RuntimeEventSink, TraceEvent};

use crate::panels::{AuditCommand, GatewayState, TraceCommand, TraceSpanDto};

/// Bus capacity: bounded, newest-first under pressure.
const BUS_CAPACITY: usize = 256;

/// One product-facing event frame.
#[derive(Debug, Clone, Serialize)]
pub struct GatewayEvent {
    /// Stable event name (see module docs).
    pub event: String,
    /// Event payload (JSON object).
    pub data: serde_json::Value,
}

impl GatewayEvent {
    pub fn new(event: &str, data: serde_json::Value) -> Self {
        Self {
            event: event.to_string(),
            data,
        }
    }
}

/// In-process event bus shared by handlers and the SSE endpoint.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<GatewayEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(BUS_CAPACITY)
    }
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish one event; no subscribers is a silent success, and a lagging
    /// subscriber never blocks the publisher.
    pub fn publish(&self, event: GatewayEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe for events emitted after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<GatewayEvent> {
        self.tx.subscribe()
    }
}

impl RuntimeEventSink for EventBus {
    fn emit(&self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::TurnStarted {
                session,
                request,
                trace,
            } => self.publish(GatewayEvent::new(
                "turn_started",
                serde_json::json!({
                    "session": session,
                    "request": request,
                    "trace_id": trace,
                }),
            )),
            RuntimeEvent::Trace {
                session,
                trace,
                at,
                event,
            } => self.emit_trace(session, trace, at, event),
            RuntimeEvent::TurnCompleted {
                session,
                request,
                trace,
                rounds,
                served_by,
            } => self.publish(GatewayEvent::new(
                "turn_completed",
                serde_json::json!({
                    "session": session,
                    "request": request,
                    "trace_id": trace,
                    "rounds": rounds,
                    "served_by": served_by,
                }),
            )),
            RuntimeEvent::ApprovalRequired {
                session,
                request,
                trace,
                approval,
                capability,
                tool_name,
                tool_call_id,
            } => self.publish(GatewayEvent::new(
                "approval_required",
                serde_json::json!({
                    "session": session,
                    "request": request,
                    "trace_id": trace,
                    "approval_id": approval,
                    "capability_id": capability,
                    "tool_name": tool_name,
                    "tool_call_id": tool_call_id,
                }),
            )),
            RuntimeEvent::TurnFailed {
                session,
                request,
                trace,
                error,
            } => self.publish(GatewayEvent::new(
                "turn_failed",
                serde_json::json!({
                    "session": session,
                    "request": request,
                    "trace_id": trace,
                    "error": error,
                }),
            )),
        }
    }
}

impl EventBus {
    fn emit_trace(
        &self,
        session: apeireth_core::kernel::SessionId,
        trace: apeireth_core::kernel::TraceId,
        _at: apeireth_core::kernel::Timestamp,
        event: TraceEvent,
    ) {
        let (name, data) = match event {
            TraceEvent::CapabilityDispatched {
                capability,
                tool_call_id,
                round,
            } => (
                "tool_started",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "capability_id": capability,
                    "tool_call_id": tool_call_id,
                    "round": round,
                }),
            ),
            TraceEvent::CapabilityCompleted {
                capability,
                tool_call_id,
                succeeded,
                round,
            } => (
                if succeeded {
                    "tool_completed"
                } else {
                    "tool_failed"
                },
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "capability_id": capability,
                    "tool_call_id": tool_call_id,
                    "succeeded": succeeded,
                    "round": round,
                }),
            ),
            TraceEvent::CapabilityUnavailable {
                requested,
                tool_call_id,
                reason,
                round,
            } => (
                "tool_failed",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "requested": requested,
                    "tool_call_id": tool_call_id,
                    "reason": reason,
                    "round": round,
                }),
            ),
            TraceEvent::ProviderInvoked {
                provider,
                model,
                round,
            } => (
                "provider_started",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "provider": provider,
                    "model": model,
                    "round": round,
                }),
            ),
            TraceEvent::ProviderSucceeded {
                provider,
                round,
                finish_reason,
                usage,
            } => (
                "provider_completed",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "provider": provider,
                    "round": round,
                    "finish_reason": finish_reason,
                    "usage": usage,
                }),
            ),
            TraceEvent::ProviderFailed {
                provider,
                round,
                error,
                retryable,
            } => (
                "provider_failed",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "provider": provider,
                    "round": round,
                    "error": error,
                    "retryable": retryable,
                }),
            ),
            TraceEvent::GovernanceEvaluated {
                hook,
                action,
                decision,
                reason,
                round,
                ..
            } => (
                "governance_evaluated",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "hook": hook,
                    "action": action,
                    "decision": decision,
                    "reason": reason,
                    "round": round,
                }),
            ),
            TraceEvent::ApprovalResolved {
                approval_id,
                decision,
                round,
            } => (
                "approval_resolved",
                serde_json::json!({
                    "session": session,
                    "trace_id": trace,
                    "approval_id": approval_id,
                    "decision": decision,
                    "round": round,
                }),
            ),
            TraceEvent::ApprovalRequested { .. } | TraceEvent::TurnCompleted { .. } => return,
            _ => return,
        };
        self.publish(GatewayEvent::new(name, data));
    }
}

/// Runtime-event consumer that archives trace/audit facts through gateway
/// ports. It collects synchronously at the runtime boundary, then flushes from
/// the request future so archive writes are awaited without blocking the
/// kernel's event sink.
pub struct RuntimeObservationSink {
    traces: Mutex<HashMap<String, Vec<TraceSpanDto>>>,
    audit: Mutex<Vec<(String, Option<String>)>>,
    trace_commands: Option<Arc<dyn TraceCommand>>,
    audit_commands: Option<Arc<dyn AuditCommand>>,
}

impl RuntimeObservationSink {
    pub fn new(
        trace_commands: Option<Arc<dyn TraceCommand>>,
        audit_commands: Option<Arc<dyn AuditCommand>>,
    ) -> Self {
        Self {
            traces: Mutex::new(HashMap::new()),
            audit: Mutex::new(Vec::new()),
            trace_commands,
            audit_commands,
        }
    }

    /// Persist all facts collected since the previous flush.
    pub async fn flush(&self) {
        let traces = self
            .traces
            .lock()
            .map(|mut traces| std::mem::take(&mut *traces))
            .unwrap_or_default();
        let audit = self
            .audit
            .lock()
            .map(|mut audit| std::mem::take(&mut *audit))
            .unwrap_or_default();

        if let Some(command) = &self.trace_commands {
            let mut traces = traces.into_iter().collect::<Vec<_>>();
            traces.sort_by(|left, right| left.0.cmp(&right.0));
            for (trace_id, spans) in traces {
                command.append_trace(&trace_id, spans).await;
            }
        }
        if let Some(command) = &self.audit_commands {
            for (event, detail) in audit {
                command.append_audit(&event, detail.as_deref()).await;
            }
        }
    }
}

impl RuntimeEventSink for RuntimeObservationSink {
    fn emit(&self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Trace {
                session,
                trace,
                at,
                event,
            } => {
                if let Ok(mut traces) = self.traces.lock() {
                    let trace_id = trace.to_string();
                    let index = traces.get(&trace_id).map_or(0, Vec::len);
                    let parent = (index > 0).then(|| format!("{trace_id}-0"));
                    let (kind, status) = trace_span_kind_status(&event);
                    traces
                        .entry(trace_id.clone())
                        .or_default()
                        .push(TraceSpanDto {
                            span_id: format!("{trace_id}-{index}"),
                            parent_span_id: parent,
                            kind: kind.to_string(),
                            actor: "runtime".to_string(),
                            status: status.to_string(),
                            summary: None,
                            started_at: at.epoch_millis(),
                            ended_at: None,
                            session_id: Some(session.to_string()),
                        });
                }
                if let TraceEvent::ApprovalResolved {
                    approval_id,
                    decision,
                    ..
                } = event
                {
                    if let Ok(mut audit) = self.audit.lock() {
                        audit.push((
                            "approval.resolved".to_string(),
                            Some(format!("approval={} decision={decision}", approval_id)),
                        ));
                    }
                }
            }
            RuntimeEvent::TurnCompleted {
                session,
                rounds,
                served_by,
                ..
            } => {
                if let Ok(mut audit) = self.audit.lock() {
                    audit.push((
                        "chat.turn.completed".to_string(),
                        Some(format!(
                            "session={session} rounds={rounds} served_by={served_by}"
                        )),
                    ));
                }
            }
            RuntimeEvent::ApprovalRequired {
                session,
                approval,
                tool_name,
                ..
            } => {
                if let Ok(mut audit) = self.audit.lock() {
                    audit.push((
                        "chat.turn.pending_approval".to_string(),
                        Some(format!(
                            "session={session} approval={approval} tool={tool_name}"
                        )),
                    ));
                }
            }
            RuntimeEvent::TurnFailed { session, error, .. } => {
                if let Ok(mut audit) = self.audit.lock() {
                    audit.push((
                        "chat.turn.failed".to_string(),
                        Some(format!("session={session} error={error}")),
                    ));
                }
            }
            RuntimeEvent::TurnStarted { .. } => {}
        }
    }
}

fn trace_span_kind_status(event: &TraceEvent) -> (&'static str, &'static str) {
    match event {
        TraceEvent::ProviderInvoked { .. } | TraceEvent::ProviderSucceeded { .. } => {
            ("provider", "ok")
        }
        TraceEvent::ProviderFailed { .. } => ("provider", "error"),
        TraceEvent::ApprovalRequested { .. } | TraceEvent::ApprovalResolved { .. } => {
            ("approval", "ok")
        }
        TraceEvent::GovernanceEvaluated { .. } => ("governance", "ok"),
        TraceEvent::CapabilityUnavailable { .. } => ("capability", "error"),
        TraceEvent::CapabilityDispatched { .. } => ("tool", "ok"),
        TraceEvent::CapabilityCompleted { succeeded, .. } => {
            ("tool", if *succeeded { "ok" } else { "error" })
        }
        TraceEvent::TurnCompleted { .. } => ("turn", "ok"),
        _ => ("event", "ok"),
    }
}

/// `GET /v1/apeireth/events` — SSE stream of gateway lifecycle events.
pub async fn events_handler(State(state): State<GatewayState>) -> Response {
    let receiver = state.events.subscribe();
    let stream = BroadcastStream::new(receiver).filter_map(|item| match item {
        Ok(event) => match serde_json::to_string(&event.data) {
            Ok(payload) => Some(Ok::<_, Infallible>(
                SseEvent::default().event(&event.event).data(payload),
            )),
            Err(_) => Some(Ok::<_, Infallible>(
                SseEvent::default().event(&event.event).data("{}"),
            )),
        },
        Err(_lagged) => None,
    });
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}
