//! Observer implementing `RuntimeEventSink` for closing the loop on Guard ML dataset collection.
//!
//! Observes runtime events (approval resolutions, capability completions, turn completions/failures)
//! and feeds them as outcome records into `DatasetRecorder`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use apeireth_guard::DatasetRecorder;
use apeireth_runtime::canonical::{RuntimeEvent, RuntimeEventSink, TraceEvent};

/// Observer implementing `RuntimeEventSink` for closing the loop on Guard ML dataset collection.
#[derive(Clone)]
pub struct GuardDatasetObserver {
    recorder: Arc<DatasetRecorder>,
    approvals: Arc<Mutex<HashMap<String, (String, String)>>>,
}

impl GuardDatasetObserver {
    /// Creates a new observer wrapping the given dataset recorder.
    pub fn new(recorder: Arc<DatasetRecorder>) -> Self {
        Self {
            recorder,
            approvals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Access the underlying dataset recorder.
    pub fn recorder(&self) -> &Arc<DatasetRecorder> {
        &self.recorder
    }
}

impl RuntimeEventSink for GuardDatasetObserver {
    fn emit(&self, event: RuntimeEvent) {
        if !self.recorder.is_enabled() {
            return;
        }

        match event {
            RuntimeEvent::ApprovalRequired {
                approval,
                tool_call_id,
                ..
            } => {
                self.approvals
                    .lock()
                    .expect("guard dataset approval map poisoned")
                    .insert(approval.to_string(), (tool_call_id.clone(), tool_call_id));
                // The request is intentionally not a training label. The
                // eventual resolution below is the approval event.
            }
            RuntimeEvent::Trace {
                session: _,
                trace,
                at: _,
                event,
            } => match event {
                TraceEvent::ApprovalResolved {
                    approval_id,
                    decision,
                    round: _,
                } => {
                    let binding = self
                        .approvals
                        .lock()
                        .expect("guard dataset approval map poisoned")
                        .remove(&approval_id.to_string());
                    if let Some((action_id, tool_call_id)) = binding {
                        self.recorder.record_approval(
                            &trace.to_string(),
                            &action_id,
                            &tool_call_id,
                            &approval_id.to_string(),
                            &decision,
                        );
                    } else {
                        self.recorder.record_outcome(
                            &trace.to_string(),
                            None,
                            None,
                            Some(&approval_id.to_string()),
                            Some(&decision),
                            None,
                        );
                    }
                }
                TraceEvent::CapabilityCompleted {
                    capability: _,
                    tool_call_id,
                    succeeded,
                    round: _,
                } => {
                    let outcome = if succeeded { "success" } else { "failure" };
                    self.recorder.record_execution(
                        &trace.to_string(),
                        &tool_call_id,
                        &tool_call_id,
                        outcome,
                    );
                }
                TraceEvent::TurnCompleted { .. } => {
                    self.recorder.record_outcome(
                        &trace.to_string(),
                        None,
                        None,
                        None,
                        None,
                        Some("turn_completed"),
                    );
                }
                _ => {}
            },
            RuntimeEvent::TurnCompleted { trace, .. } => {
                self.recorder.record_outcome(
                    &trace.to_string(),
                    None,
                    None,
                    None,
                    None,
                    Some("turn_completed"),
                );
            }
            RuntimeEvent::TurnFailed { trace, error, .. } => {
                self.recorder.record_outcome(
                    &trace.to_string(),
                    None,
                    None,
                    None,
                    None,
                    Some(&format!("turn_failed: {error}")),
                );
            }
            _ => {}
        }
    }
}
