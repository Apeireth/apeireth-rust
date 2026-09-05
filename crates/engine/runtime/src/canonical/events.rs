//! Runtime-owned event spine.
//!
//! The kernel publishes facts about execution; adapters decide how those facts
//! are transported or rendered. Events carry identifiers, status, and
//! structured trace data only. They never carry model reasoning or credentials.

use std::sync::Arc;

use apeireth_core::kernel::{ApprovalId, CapabilityId, RequestId, SessionId, Timestamp, TraceId};

use super::approval::PendingApprovalView;
use super::trace::TraceEvent;

/// One canonical runtime event emitted at the execution boundary.
#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    /// A turn acquired its runtime request and trace identities.
    TurnStarted {
        session: SessionId,
        request: RequestId,
        trace: TraceId,
    },
    /// One structured action from the execution trace.
    Trace {
        session: SessionId,
        trace: TraceId,
        at: Timestamp,
        event: TraceEvent,
    },
    /// A turn committed an assistant response.
    TurnCompleted {
        session: SessionId,
        request: RequestId,
        trace: TraceId,
        rounds: u32,
        served_by: CapabilityId,
    },
    /// A turn is waiting for a human decision.
    ApprovalRequired {
        session: SessionId,
        request: RequestId,
        trace: TraceId,
        approval: ApprovalId,
        capability: CapabilityId,
        tool_name: String,
        /// Stable provider tool-call identity for dataset/audit correlation.
        tool_call_id: String,
    },
    /// A turn failed before it could commit a final response.
    TurnFailed {
        session: SessionId,
        request: RequestId,
        trace: TraceId,
        error: String,
    },
}

/// Sink implemented by transports, telemetry, or host applications.
pub trait RuntimeEventSink: Send + Sync {
    /// Consume one event. Sinks must be non-blocking and must not influence
    /// the runtime result.
    fn emit(&self, event: RuntimeEvent);
}

/// A sink that deliberately discards events.
#[derive(Debug, Default)]
pub struct NoopRuntimeEventSink;

impl RuntimeEventSink for NoopRuntimeEventSink {
    fn emit(&self, _event: RuntimeEvent) {}
}

/// Fan-out sink used when one host wants the same runtime fact in more than
/// one observation channel (for example gateway SSE plus an audit adapter).
#[derive(Default)]
pub struct CompositeRuntimeEventSink {
    sinks: Vec<Arc<dyn RuntimeEventSink>>,
}

impl CompositeRuntimeEventSink {
    /// Compose an ordered set of sinks. Emission order is deterministic.
    pub fn new(sinks: Vec<Arc<dyn RuntimeEventSink>>) -> Self {
        Self { sinks }
    }

    /// Add one sink to the end of the fan-out list.
    pub fn push(&mut self, sink: Arc<dyn RuntimeEventSink>) {
        self.sinks.push(sink);
    }
}

impl RuntimeEventSink for CompositeRuntimeEventSink {
    fn emit(&self, event: RuntimeEvent) {
        for sink in &self.sinks {
            sink.emit(event.clone());
        }
    }
}

/// Short public spelling for the runtime fan-out sink.
pub type CompositeEventSink = CompositeRuntimeEventSink;

/// Convenience adapter for a closure-owned event sink.
pub struct FnRuntimeEventSink<F>(pub F);

impl<F> RuntimeEventSink for FnRuntimeEventSink<F>
where
    F: Fn(RuntimeEvent) + Send + Sync,
{
    fn emit(&self, event: RuntimeEvent) {
        (self.0)(event);
    }
}

/// Make a sink from a closure without exposing an adapter type at call sites.
pub fn event_sink<F>(f: F) -> Arc<dyn RuntimeEventSink>
where
    F: Fn(RuntimeEvent) + Send + Sync + 'static,
{
    Arc::new(FnRuntimeEventSink(f))
}

/// Convert a pending view into the event's stable fields.
pub(crate) fn approval_event_fields(
    view: &PendingApprovalView,
) -> (
    SessionId,
    RequestId,
    TraceId,
    ApprovalId,
    CapabilityId,
    String,
) {
    (
        view.session_id,
        view.request_id,
        view.trace_id,
        view.approval_id,
        view.capability_id.clone(),
        view.tool_name.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use apeireth_core::kernel::{RequestId, SessionId, TraceId};
    use std::sync::Mutex;

    #[test]
    fn composite_sink_fans_out_without_changing_the_event() {
        let first = Arc::new(Mutex::new(0usize));
        let second = Arc::new(Mutex::new(0usize));
        let mut sink = CompositeEventSink::default();
        let first_seen = Arc::clone(&first);
        let second_seen = Arc::clone(&second);
        sink.push(event_sink(move |_| *first_seen.lock().unwrap() += 1));
        sink.push(event_sink(move |_| *second_seen.lock().unwrap() += 1));

        sink.emit(RuntimeEvent::TurnStarted {
            session: SessionId::new(),
            request: RequestId::new(),
            trace: TraceId::new(),
        });

        assert_eq!(*first.lock().unwrap(), 1);
        assert_eq!(*second.lock().unwrap(), 1);
    }
}
