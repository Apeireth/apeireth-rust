//! The agent loop: the runtime's single semantic entry point.
//!
//! # The chain
//!
//! ```text
//!   user request
//!        v
//!   normalized request  <-- session transcript + active tool declarations
//!        v
//!   governance (completion)
//!        v
//!   provider router --> provider
//!        v
//!   tool calls?  -- no --> final response
//!        | yes
//!        v
//!   governance (dispatch) --> capability lookup --> plugin dispatch
//!        v
//!   tool result -> transcript -> provider again
//! ```
//!
//! Everything above happens here and nowhere else. A gateway or CLI that runs
//! its own version of this chain is a second runtime, and the two will diverge
//! at the first behaviour change.
//!
//! # Approval is not an error
//!
//! `Runtime::execute_outcome` returns a [`TurnOutcome`]: either a completed
//! turn or a pending approval. `Runtime::execute` is a compatibility wrapper
//! that maps a pending approval back to the old `RuntimeError::ApprovalRequired`
//! for callers that have not adopted the canonical outcome model yet. New
//! callers should use `execute_outcome` and `resolve_approval`.
//!
//! # Why a tool failure is not a turn failure
//!
//! Three things can go wrong with a tool call, and all three produce a
//! [`ToolResult`] handed back to the model rather than an aborted turn: the
//! model named a tool that does not exist, governance refused the call, or the
//! tool itself failed. In each case the model is told what happened and gets to
//! respond — which is the entire point of a loop. Aborting would discard a turn
//! the model could have recovered from, and hide the reason from the only party
//! able to act on it.
//!
//! Two things do abort: governance denying the *completion* itself, and the
//! round limit. Neither is something the model can recover from by trying again.

use std::sync::Arc;

use apeireth_core::kernel::{ApprovalId, CapabilityId, RequestId, SessionId, Timestamp, TraceId};
use apeireth_governance::{Action, Decision, GovernanceRequest};
use apeireth_plugin::FrozenInvocation;
use apeireth_protocol::canonical::{
    NormalizedMessage, NormalizedRequest, NormalizedResponse, NormalizedTool, NormalizedUsage,
    ToolCall, ToolResult,
};

use super::approval::{
    operation_fingerprint_with_invocation, ApprovalDecision, ApprovalStatus,
    FrozenTurnContinuation, PendingApproval, PendingApprovalView,
};
use super::error::{RuntimeError, RuntimeResult};
use super::events::RuntimeEvent;
use super::module::{
    HookPoint, InvocationContext, ModuleContext, ModuleDirective, ModuleInvoker, ModuleOutcome,
    ModuleTurnState, PromptOverlay,
};
use super::runtime::Runtime;
use super::session::{Session, SessionEventKind};
use super::subloop::RuntimeSubLoopSpawner;
use super::trace::{ExecutionTrace, TraceEvent};

/// One turn's input.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// The conversation to continue. A session that does not exist is created.
    pub session: SessionId,
    /// What the user said.
    pub input: String,
    /// Model to use, or the runtime's default when absent.
    pub model: Option<String>,
    /// System instruction, applied only when the transcript is empty.
    ///
    /// Applied once rather than per turn so that a resumed session does not
    /// accumulate duplicate system messages, which quietly change behaviour and
    /// cost tokens on every subsequent request.
    pub system: Option<String>,
}

impl TurnRequest {
    /// A turn against `session` saying `input`.
    pub fn new(session: SessionId, input: impl Into<String>) -> Self {
        Self {
            session,
            input: input.into(),
            model: None,
            system: None,
        }
    }

    /// Use a specific model for this turn.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Seed a new session with a system instruction.
    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }
}

/// One turn's outcome.
///
/// Note the absence of any raw-reasoning field. What the turn *did* is in
/// [`TurnResponse::trace`]; see [`super::trace`].
#[derive(Debug, Clone)]
pub struct TurnResponse {
    /// The conversation this belongs to.
    pub session: SessionId,
    /// This turn's request id.
    pub request: RequestId,
    /// The assistant's final text.
    pub text: String,
    /// The provider that produced the final response.
    pub served_by: CapabilityId,
    /// Token accounting for the final response.
    pub usage: NormalizedUsage,
    /// How many logical execution rounds the turn took.
    ///
    /// A module retry consumes one round-budget slot even when it prevents a
    /// provider request from being sent in that slot.
    pub rounds: u32,
    /// Everything the runtime did, in order.
    pub trace: ExecutionTrace,
}

/// The result of running one turn: it either completed or paused for human
/// approval.
#[derive(Debug, Clone)]
pub enum TurnOutcome {
    /// The turn reached a final assistant response.
    Completed(TurnResponse),
    /// The turn is suspended until a human resolves the returned approval.
    PendingApproval(PendingApprovalView),
}

impl TurnOutcome {
    /// The completed response, when the turn completed.
    pub fn completed(self) -> Option<TurnResponse> {
        match self {
            Self::Completed(response) => Some(response),
            Self::PendingApproval(_) => None,
        }
    }
}

/// The result of resolving one pending approval.
#[derive(Debug, Clone)]
pub enum ApprovalResolution {
    /// The resolution was accepted and the turn resumed. This may complete the
    /// turn or pause again on a later tool call.
    Resumed(TurnOutcome),
    /// The approval had already reached a final (non-resumable) state.
    AlreadyResolved { status: ApprovalStatus },
    /// The approval is claimed but no `Consumed` result was ever recorded. The
    /// external effect may or may not have happened; automatic retry is unsafe.
    ExecutionInterrupted { approval_id: ApprovalId },
    /// The approval expired before it was resolved.
    Expired,
    /// The approval id is unknown for this session.
    NotFound,
}

enum ToolDispatch {
    Result(ToolResult),
    Pending {
        capability_id: CapabilityId,
        tool_name: String,
        tool_call: ToolCall,
        effective_invocation: Option<FrozenInvocation>,
        governance_hook: String,
        governance_reason: String,
    },
}

/// Deterministic aggregate of one hook's module outcomes.
///
/// Modules run in registration order, overlays are concatenated in that order,
/// and the strongest directive wins (`Stop` > `Retry` > `Continue`). Ties keep
/// the first module's directive and module id. Every module is still invoked for
/// the hook; a directive only controls the canonical loop after the hook batch
/// has completed.
struct HookEffects {
    prompt_overlays: Vec<PromptOverlay>,
    directive: ModuleDirective,
    directive_module_id: Option<String>,
}

impl Default for HookEffects {
    fn default() -> Self {
        Self {
            prompt_overlays: Vec::new(),
            directive: ModuleDirective::Continue,
            directive_module_id: None,
        }
    }
}

impl HookEffects {
    fn push(&mut self, module_id: &str, outcome: ModuleOutcome) {
        self.prompt_overlays.extend(outcome.prompt_overlays);

        let incoming_strength = directive_strength(&outcome.directive);
        let current_strength = directive_strength(&self.directive);
        if incoming_strength > current_strength {
            self.directive = outcome.directive;
            self.directive_module_id = Some(module_id.to_string());
        }
    }
}

fn directive_strength(directive: &ModuleDirective) -> u8 {
    match directive {
        ModuleDirective::Continue => 0,
        ModuleDirective::Retry { .. } => 1,
        ModuleDirective::Stop { .. } => 2,
    }
}

fn compose_provider_messages(
    session_messages: &[NormalizedMessage],
    retry_scaffolding: &[NormalizedMessage],
    overlays: &[PromptOverlay],
) -> Vec<NormalizedMessage> {
    let mut messages =
        Vec::with_capacity(overlays.len() + session_messages.len() + retry_scaffolding.len());
    messages.extend(overlays.iter().map(|overlay| overlay.message().clone()));
    messages.extend_from_slice(session_messages);
    messages.extend_from_slice(retry_scaffolding);
    messages
}

impl Runtime {
    /// Run one turn to completion or pending approval.
    ///
    /// This is the canonical outcome-model entry point. CLI, gateway, desktop
    /// and tests should migrate to this; [`Runtime::execute`] is the
    /// compatibility wrapper.
    pub async fn execute_outcome(&self, request: TurnRequest) -> RuntimeResult<TurnOutcome> {
        let lock = self.session_locks.acquire(request.session).await;
        let _guard = lock.lock().await;

        let state = Arc::new(ModuleTurnState::new(self.config.max_module_invocations));
        let request_id = RequestId::new();
        let trace_id = TraceId::new();
        self.emit_event(RuntimeEvent::TurnStarted {
            session: request.session,
            request: request_id,
            trace: trace_id,
        });
        let observed_request = request.clone();
        let result = self
            .execute_outcome_locked(request, state.clone(), request_id, trace_id)
            .await;
        if let Err(error) = &result {
            self.observe_error(&observed_request, state, error).await;
        }
        self.emit_outcome_events(observed_request.session, request_id, trace_id, &result);
        result
    }

    async fn execute_outcome_locked(
        &self,
        request: TurnRequest,
        module_state: Arc<ModuleTurnState>,
        request_id: RequestId,
        trace_id: TraceId,
    ) -> RuntimeResult<TurnOutcome> {
        let mut trace = ExecutionTrace::new(trace_id, request.session, request_id);

        let clock = self.clock.as_ref();
        let mut session = self.sessions.load_or_create(request.session).await?;
        self.expire_active_approval_if_needed(&mut session).await?;

        if let Some(active) = session.active_approval_id {
            return Err(RuntimeError::SessionApprovalPending {
                session: request.session,
                approval: active,
            });
        }

        if session.is_empty() {
            if let Some(system) = &request.system {
                session.append(NormalizedMessage::system(system.clone()), clock);
            }
        }
        session.append(NormalizedMessage::user(request.input.clone()), clock);
        session.record(request_id, trace_id, SessionEventKind::TurnStarted, clock);
        self.sessions.save(&session).await?;

        let Some(model) = request
            .model
            .clone()
            .or_else(|| self.config.default_model.clone())
        else {
            let error = RuntimeError::misconfigured(
                "no model: the turn named none and the runtime has no default_model",
            );
            session.record(
                request_id,
                trace_id,
                SessionEventKind::ExecutionFailed {
                    phase: "model_selection".into(),
                    error: error.to_string(),
                },
                clock,
            );
            self.sessions.save(&session).await?;
            return Err(error);
        };

        let invocation = InvocationContext::user_turn();
        let turn_start_messages = session.messages.clone();
        let turn_start = self
            .run_hook_checked(
                HookPoint::TurnStart,
                &mut session,
                &request.session,
                &invocation,
                &model,
                &turn_start_messages,
                None,
                None,
                None,
                None,
                request_id,
                trace_id,
                &module_state,
            )
            .await?;

        let initial_retry = match turn_start.directive {
            ModuleDirective::Continue => Vec::new(),
            ModuleDirective::Retry { feedback } => {
                vec![NormalizedMessage::user(feedback)]
            }
            ModuleDirective::Stop { reason } => {
                let module_id = turn_start
                    .directive_module_id
                    .as_deref()
                    .unwrap_or("unknown");
                return self
                    .fail_module_stop(&mut session, request_id, trace_id, module_id, reason)
                    .await;
            }
        };

        let tools = self.tool_declarations();
        let continuation =
            FrozenTurnContinuation::start_of_round(request_id, trace_id, model.clone(), 1);

        self.advance(
            session,
            trace,
            request.session,
            request_id,
            trace_id,
            tools,
            continuation,
            initial_retry,
            turn_start.prompt_overlays,
            invocation,
            module_state,
        )
        .await
    }

    /// Compatibility wrapper: run one turn and return the completed response.
    ///
    /// A pending approval is mapped to [`RuntimeError::ApprovalRequired`] so
    /// callers that have not adopted [`Runtime::execute_outcome`] keep their
    /// old behaviour. The semantic engine is the same. The error retains the
    /// [`ApprovalId`] so a caller can still resume without a second subsystem.
    pub async fn execute(&self, request: TurnRequest) -> RuntimeResult<TurnResponse> {
        match self.execute_outcome(request).await? {
            TurnOutcome::Completed(response) => Ok(response),
            TurnOutcome::PendingApproval(view) => Err(RuntimeError::ApprovalRequired {
                hook: view.governance_hook,
                reason: view.governance_reason,
                approval: Some(view.approval_id),
                session: Some(view.session_id),
            }),
        }
    }

    /// Resolve a pending approval for one session.
    ///
    /// The resolver supplies only a decision and an optional human reason. It
    /// never supplies replacement tool arguments, cwd, script text, or process
    /// configuration. The frozen operation is executed exactly as stored.
    pub async fn resolve_approval(
        &self,
        session_id: SessionId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    ) -> RuntimeResult<ApprovalResolution> {
        let lock = self.session_locks.acquire(session_id).await;
        let _guard = lock.lock().await;

        let event_ids = self
            .sessions
            .load(&session_id)
            .await
            .ok()
            .flatten()
            .and_then(|session| {
                session
                    .approvals
                    .get(&approval_id)
                    .map(|approval| (approval.request_id, approval.trace_id))
            })
            .unwrap_or_else(|| (RequestId::new(), TraceId::new()));
        let state = Arc::new(ModuleTurnState::new(self.config.max_module_invocations));
        let request = TurnRequest::new(session_id, "");
        let result = self
            .resolve_approval_locked(session_id, approval_id, decision, Arc::clone(&state))
            .await;
        if let Err(error) = &result {
            self.observe_error(&request, state, error).await;
        }
        match &result {
            Ok(ApprovalResolution::Resumed(outcome)) => {
                self.emit_outcome_events(
                    session_id,
                    event_ids.0,
                    event_ids.1,
                    &Ok(outcome.clone()),
                );
            }
            Ok(_) => {}
            Err(error) => self.emit_event(RuntimeEvent::TurnFailed {
                session: session_id,
                request: event_ids.0,
                trace: event_ids.1,
                error: error.to_string(),
            }),
        }
        result
    }

    async fn resolve_approval_locked(
        &self,
        session_id: SessionId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        module_state: Arc<ModuleTurnState>,
    ) -> RuntimeResult<ApprovalResolution> {
        let Some(mut session) = self.sessions.load(&session_id).await? else {
            return Ok(ApprovalResolution::NotFound);
        };

        let Some(approval) = session.approvals.get(&approval_id).cloned() else {
            return Ok(ApprovalResolution::NotFound);
        };

        module_state.set_used(approval.continuation.module_invocations);

        if approval.status == ApprovalStatus::Claimed {
            return Ok(ApprovalResolution::ExecutionInterrupted { approval_id });
        }

        if approval.status != ApprovalStatus::Pending {
            return Ok(ApprovalResolution::AlreadyResolved {
                status: approval.status,
            });
        }

        let now = Timestamp::from_clock(self.clock.as_ref());
        if approval.is_expired(now) {
            let expired = {
                let mut expired = approval.clone();
                expired.status = ApprovalStatus::Expired;
                expired.human_reason = None;
                expired
            };
            session.approvals.insert(approval_id, expired);
            session.active_approval_id = None;
            session.record(
                approval.request_id,
                approval.trace_id,
                SessionEventKind::ApprovalResolved {
                    approval_id,
                    decision: "expired".into(),
                    round: approval.round,
                    human_reason: None,
                },
                self.clock.as_ref(),
            );
            // Expiration permanently ends the paused turn. Close the frozen
            // assistant tool-call batch so a later turn cannot send an
            // orphaned tool call back to a provider.
            Self::append_skipped_tool_results(
                &mut session,
                &approval.continuation.tool_calls,
                approval.continuation.next_tool_index,
                "operation expired before tool dispatch",
                self.clock.as_ref(),
            );
            self.sessions.save(&session).await?;
            return Ok(ApprovalResolution::Expired);
        }

        let decision_label = decision.label();
        match decision {
            ApprovalDecision::Reject { reason } | ApprovalDecision::Cancel { reason } => {
                let result_text = if decision_label == "cancelled" {
                    "operation cancelled by user"
                } else {
                    "operation rejected by user"
                };
                let rejected = {
                    let mut rejected = approval.clone();
                    rejected.status = ApprovalStatus::Rejected;
                    rejected.human_reason = reason.clone();
                    rejected
                };
                session.approvals.insert(approval_id, rejected);
                session.active_approval_id = None;
                session.record(
                    approval.request_id,
                    approval.trace_id,
                    SessionEventKind::ApprovalResolved {
                        approval_id,
                        decision: decision_label.into(),
                        round: approval.round,
                        human_reason: reason,
                    },
                    self.clock.as_ref(),
                );

                // The model gets a canonical rejection result and may recover.
                let rejection = ToolResult::permanent_error(&approval.tool_call.id, result_text)
                    .with_name(&approval.tool_call.name);
                session.append(rejection.clone().into_message(), self.clock.as_ref());

                let mut continuation = approval.continuation.clone();
                continuation.next_tool_index = continuation.next_tool_index.saturating_add(1);
                continuation.approved_tool_index = None;
                continuation.approved_approval_id = None;

                let after_tool_messages = session.messages.clone();
                let after_tool = self
                    .run_hook_checked(
                        HookPoint::AfterToolResult,
                        &mut session,
                        &session_id,
                        &InvocationContext::user_turn(),
                        &continuation.model,
                        &after_tool_messages,
                        None,
                        Some(&approval.tool_call),
                        Some(&rejection),
                        None,
                        approval.request_id,
                        approval.trace_id,
                        &module_state,
                    )
                    .await?;
                let mut retry_scaffolding = Vec::new();
                let pending_overlays = after_tool.prompt_overlays;
                match after_tool.directive {
                    ModuleDirective::Continue => {}
                    ModuleDirective::Retry { feedback } => {
                        Self::append_skipped_tool_results(
                            &mut session,
                            &continuation.tool_calls,
                            continuation.next_tool_index,
                            "remaining tool calls skipped by module after rejected result",
                            self.clock.as_ref(),
                        );
                        retry_scaffolding = vec![NormalizedMessage::user(feedback)];
                        continuation.tool_calls.clear();
                        continuation.next_tool_index = 0;
                        continuation.round += 1;
                    }
                    ModuleDirective::Stop { reason } => {
                        Self::append_skipped_tool_results(
                            &mut session,
                            &continuation.tool_calls,
                            continuation.next_tool_index,
                            "remaining tool calls stopped by module after rejected result",
                            self.clock.as_ref(),
                        );
                        return match self
                            .fail_module_stop(
                                &mut session,
                                approval.request_id,
                                approval.trace_id,
                                after_tool
                                    .directive_module_id
                                    .as_deref()
                                    .unwrap_or("unknown"),
                                reason,
                            )
                            .await
                        {
                            Err(error) => Err(error),
                            Ok(_) => unreachable!("module stop cannot complete a turn"),
                        };
                    }
                }

                let tools = self.tool_declarations();
                let mut trace =
                    ExecutionTrace::new(approval.trace_id, session_id, approval.request_id);
                trace.record(
                    now,
                    TraceEvent::ApprovalResolved {
                        approval_id,
                        decision: decision_label.into(),
                        round: approval.round,
                    },
                );

                let outcome = self
                    .advance(
                        session,
                        trace,
                        session_id,
                        approval.request_id,
                        approval.trace_id,
                        tools,
                        continuation,
                        retry_scaffolding,
                        pending_overlays,
                        InvocationContext::user_turn(),
                        Arc::clone(&module_state),
                    )
                    .await?;
                Ok(ApprovalResolution::Resumed(outcome))
            }
            ApprovalDecision::Approve => {
                let claimed = {
                    let mut claimed = approval.clone();
                    claimed.status = ApprovalStatus::Claimed;
                    claimed.human_reason = None;
                    claimed
                };
                session.approvals.insert(approval_id, claimed);
                // Keep the session blocked on this approval until the consumed
                // result has been durably written after execution. If the
                // process dies after this save, a restart must observe Claimed
                // and refuse to re-execute.
                session.active_approval_id = Some(approval_id);
                session.record(
                    approval.request_id,
                    approval.trace_id,
                    SessionEventKind::ApprovalResolved {
                        approval_id,
                        decision: "approved".into(),
                        round: approval.round,
                        human_reason: None,
                    },
                    self.clock.as_ref(),
                );

                // Claim-before-effect: the claimed state MUST be persisted
                // before the approved tool is invoked.
                self.sessions.save(&session).await?;

                let mut continuation = approval.continuation.clone();
                continuation.approved_tool_index = Some(continuation.next_tool_index);
                continuation.approved_approval_id = Some(approval_id);

                let tools = self.tool_declarations();
                let mut trace =
                    ExecutionTrace::new(approval.trace_id, session_id, approval.request_id);
                trace.record(
                    now,
                    TraceEvent::ApprovalResolved {
                        approval_id,
                        decision: "approved".into(),
                        round: approval.round,
                    },
                );

                let outcome = self
                    .advance(
                        session,
                        trace,
                        session_id,
                        approval.request_id,
                        approval.trace_id,
                        tools,
                        continuation,
                        Vec::new(),
                        Vec::new(),
                        InvocationContext::user_turn(),
                        Arc::clone(&module_state),
                    )
                    .await?;
                Ok(ApprovalResolution::Resumed(outcome))
            }
        }
    }

    /// The single turn state machine.
    ///
    /// It starts a new round when `continuation.tool_calls` is empty, and
    /// resumes mid-round when a pending approval froze the original tool-call
    /// batch. The original provider tool call is never regenerated.
    async fn advance(
        &self,
        mut session: Session,
        mut trace: ExecutionTrace,
        session_id: SessionId,
        request_id: RequestId,
        trace_id: TraceId,
        tools: Vec<NormalizedTool>,
        mut continuation: FrozenTurnContinuation,
        mut retry_scaffolding: Vec<NormalizedMessage>,
        mut pending_overlays: Vec<PromptOverlay>,
        invocation: InvocationContext,
        module_state: Arc<ModuleTurnState>,
    ) -> RuntimeResult<TurnOutcome> {
        let clock = self.clock.as_ref();

        loop {
            let request_overlays = std::mem::take(&mut pending_overlays);
            let mut next_overlays = if continuation.tool_calls.is_empty() {
                Vec::new()
            } else {
                request_overlays.clone()
            };
            let mut current_candidate: Option<NormalizedResponse> = None;

            if continuation.tool_calls.is_empty() {
                if continuation.round > self.config.max_rounds {
                    let error = RuntimeError::RoundLimitExceeded {
                        limit: self.config.max_rounds,
                    };
                    session.record(
                        request_id,
                        trace_id,
                        SessionEventKind::ExecutionFailed {
                            phase: "round_limit".into(),
                            error: error.to_string(),
                        },
                        clock,
                    );
                    self.sessions.save(&session).await?;
                    return Err(error);
                }

                let model_messages =
                    compose_provider_messages(&session.messages, &retry_scaffolding, &[]);
                let before_model = self
                    .run_hook_checked(
                        HookPoint::BeforeModelCall,
                        &mut session,
                        &session_id,
                        &invocation,
                        &continuation.model,
                        &model_messages,
                        None,
                        None,
                        None,
                        None,
                        request_id,
                        trace_id,
                        &module_state,
                    )
                    .await?;
                let before_model_overlays = before_model.prompt_overlays;

                match before_model.directive {
                    ModuleDirective::Continue => {}
                    ModuleDirective::Retry { feedback } => {
                        retry_scaffolding = vec![NormalizedMessage::user(feedback)];
                        // Nothing was sent to a provider in this attempt. Any
                        // overlay staged for that attempt is therefore spent;
                        // the next BeforeModelCall hook recomputes its own
                        // transient request overlay.
                        pending_overlays = Vec::new();
                        continuation.round += 1;
                        continue;
                    }
                    ModuleDirective::Stop { reason } => {
                        return self
                            .fail_module_stop(
                                &mut session,
                                request_id,
                                trace_id,
                                before_model
                                    .directive_module_id
                                    .as_deref()
                                    .unwrap_or("unknown"),
                                reason,
                            )
                            .await;
                    }
                }

                if let Err(error) = self
                    .authorize_completion(
                        &mut trace,
                        &session_id,
                        request_id,
                        trace_id,
                        &continuation.model,
                        &mut session,
                        continuation.round,
                    )
                    .await
                {
                    self.sessions.save(&session).await?;
                    return Err(error);
                }

                let mut provider_overlays = request_overlays;
                provider_overlays.extend(before_model_overlays);
                let provider_request = NormalizedRequest::new(
                    continuation.model.clone(),
                    compose_provider_messages(
                        &session.messages,
                        &retry_scaffolding,
                        &provider_overlays,
                    ),
                );
                retry_scaffolding.clear();
                let routed = self
                    .providers
                    .complete_with_tools(&provider_request, &tools)
                    .await;

                let routed = match routed {
                    Ok(routed) => routed,
                    Err(e) => {
                        session.record(
                            request_id,
                            trace_id,
                            SessionEventKind::ProviderFailed {
                                error: e.to_string(),
                                round: continuation.round,
                            },
                            clock,
                        );
                        self.sessions.save(&session).await?;
                        return Err(e);
                    }
                };

                for (provider, error) in &routed.failed_attempts {
                    let at = Timestamp::from_clock(clock);
                    trace.record(
                        at,
                        TraceEvent::ProviderInvoked {
                            provider: provider.clone(),
                            model: continuation.model.clone(),
                            round: continuation.round,
                        },
                    );
                    trace.record(
                        at,
                        TraceEvent::ProviderFailed {
                            provider: provider.clone(),
                            round: continuation.round,
                            error: error.to_string(),
                            retryable: error.is_retryable(),
                        },
                    );
                }

                let response = routed.response;
                let served_by = routed.served_by;
                trace.record(
                    Timestamp::from_clock(clock),
                    TraceEvent::ProviderInvoked {
                        provider: served_by.clone(),
                        model: continuation.model.clone(),
                        round: continuation.round,
                    },
                );
                trace.record(
                    Timestamp::from_clock(clock),
                    TraceEvent::ProviderSucceeded {
                        provider: served_by.clone(),
                        round: continuation.round,
                        finish_reason: response.finish_reason,
                        usage: response.usage.clone(),
                    },
                );

                let after_model_messages = session.messages.clone();
                let after_model = self
                    .run_hook_checked(
                        HookPoint::AfterModelResponse,
                        &mut session,
                        &session_id,
                        &invocation,
                        &continuation.model,
                        &after_model_messages,
                        Some(&response),
                        None,
                        None,
                        None,
                        request_id,
                        trace_id,
                        &module_state,
                    )
                    .await?;
                next_overlays.extend(after_model.prompt_overlays);

                match after_model.directive {
                    ModuleDirective::Continue => {}
                    ModuleDirective::Retry { feedback } => {
                        retry_scaffolding = vec![NormalizedMessage::user(feedback)];
                        pending_overlays = next_overlays;
                        continuation.round += 1;
                        continue;
                    }
                    ModuleDirective::Stop { reason } => {
                        return self
                            .fail_module_stop(
                                &mut session,
                                request_id,
                                trace_id,
                                after_model
                                    .directive_module_id
                                    .as_deref()
                                    .unwrap_or("unknown"),
                                reason,
                            )
                            .await;
                    }
                }

                if response.tool_calls.is_empty() {
                    let final_messages = session.messages.clone();
                    let before_final = self
                        .run_hook_checked(
                            HookPoint::BeforeFinalCommit,
                            &mut session,
                            &session_id,
                            &invocation,
                            &continuation.model,
                            &final_messages,
                            Some(&response),
                            None,
                            None,
                            None,
                            request_id,
                            trace_id,
                            &module_state,
                        )
                        .await?;
                    next_overlays.extend(before_final.prompt_overlays);

                    match before_final.directive {
                        ModuleDirective::Continue => {
                            let candidate = response.clone();
                            let turn_response = self
                                .finish_turn(
                                    &mut session,
                                    trace,
                                    request_id,
                                    served_by,
                                    response,
                                    continuation.round,
                                )
                                .await?;
                            let after_turn_messages = session.messages.clone();
                            // AfterTurn is observational: the turn is already
                            // durably committed, so its directive cannot undo it.
                            let _ = self
                                .run_hook_checked(
                                    HookPoint::AfterTurn,
                                    &mut session,
                                    &session_id,
                                    &invocation,
                                    &continuation.model,
                                    &after_turn_messages,
                                    Some(&candidate),
                                    None,
                                    None,
                                    None,
                                    request_id,
                                    trace_id,
                                    &module_state,
                                )
                                .await?;
                            return Ok(TurnOutcome::Completed(turn_response));
                        }
                        ModuleDirective::Retry { feedback } => {
                            retry_scaffolding = vec![NormalizedMessage::user(feedback)];
                            pending_overlays = next_overlays;
                            continuation.round += 1;
                            continue;
                        }
                        ModuleDirective::Stop { reason } => {
                            return self
                                .fail_module_stop(
                                    &mut session,
                                    request_id,
                                    trace_id,
                                    before_final
                                        .directive_module_id
                                        .as_deref()
                                        .unwrap_or("unknown"),
                                    reason,
                                )
                                .await;
                        }
                    }
                }

                // The assistant tool-call message must reach the transcript
                // before results, or the provider sees answers to questions it
                // never asked.
                current_candidate = Some(response.clone());
                session.append(
                    NormalizedMessage::assistant_with_tool_calls(
                        response.content,
                        response.tool_calls.clone(),
                    ),
                    clock,
                );

                continuation.tool_calls = response.tool_calls;
                continuation.next_tool_index = 0;
                continuation.approved_tool_index = None;
                continuation.approved_approval_id = None;
            }

            let mut retry_requested = None;
            while continuation.next_tool_index < continuation.tool_calls.len() {
                let index = continuation.next_tool_index;
                let call = continuation.tool_calls[index].clone();
                let is_preapproved = continuation.approved_tool_index == Some(index);
                let approved_approval = if is_preapproved {
                    let approval_id = continuation.approved_approval_id.ok_or_else(|| {
                        RuntimeError::misconfigured(
                            "approved_tool_index requires approved_approval_id",
                        )
                    })?;
                    Some(
                        session
                            .approvals
                            .get(&approval_id)
                            .cloned()
                            .ok_or_else(|| {
                                RuntimeError::misconfigured(
                                    "approved approval must exist in session",
                                )
                            })?,
                    )
                } else {
                    None
                };

                let before_tool_messages = session.messages.clone();
                let before_tool = self
                    .run_hook_checked(
                        HookPoint::BeforeToolCall,
                        &mut session,
                        &session_id,
                        &invocation,
                        &continuation.model,
                        &before_tool_messages,
                        current_candidate.as_ref(),
                        Some(&call),
                        None,
                        None,
                        request_id,
                        trace_id,
                        &module_state,
                    )
                    .await;
                let before_tool = match before_tool {
                    Ok(effects) => effects,
                    Err(error) => {
                        Self::append_skipped_tool_results(
                            &mut session,
                            &continuation.tool_calls,
                            index,
                            "module hook failed before tool dispatch",
                            clock,
                        );
                        self.sessions.save(&session).await?;
                        return Err(error);
                    }
                };
                next_overlays.extend(before_tool.prompt_overlays);

                match before_tool.directive {
                    ModuleDirective::Continue => {}
                    ModuleDirective::Retry { feedback } => {
                        if is_preapproved {
                            if let Some(approval_id) = continuation.approved_approval_id {
                                self.interrupt_claimed_approval(
                                    &mut session,
                                    approval_id,
                                    request_id,
                                    trace_id,
                                    continuation.round,
                                    clock,
                                );
                            }
                        }
                        Self::append_skipped_tool_results(
                            &mut session,
                            &continuation.tool_calls,
                            index,
                            "tool call skipped by module before dispatch",
                            clock,
                        );
                        retry_requested = Some(feedback);
                        break;
                    }
                    ModuleDirective::Stop { reason } => {
                        if is_preapproved {
                            if let Some(approval_id) = continuation.approved_approval_id {
                                self.interrupt_claimed_approval(
                                    &mut session,
                                    approval_id,
                                    request_id,
                                    trace_id,
                                    continuation.round,
                                    clock,
                                );
                            }
                        }
                        Self::append_skipped_tool_results(
                            &mut session,
                            &continuation.tool_calls,
                            index,
                            "tool call stopped by module before dispatch",
                            clock,
                        );
                        return self
                            .fail_module_stop(
                                &mut session,
                                request_id,
                                trace_id,
                                before_tool
                                    .directive_module_id
                                    .as_deref()
                                    .unwrap_or("unknown"),
                                reason,
                            )
                            .await;
                    }
                }

                match self
                    .dispatch_one_tool(
                        &mut trace,
                        &mut session,
                        &session_id,
                        request_id,
                        trace_id,
                        &call,
                        continuation.round,
                        is_preapproved,
                        approved_approval.as_ref(),
                    )
                    .await?
                {
                    ToolDispatch::Result(result) => {
                        session.append(result.clone().into_message(), clock);

                        if let Some(approved_approval_id) = continuation.approved_approval_id {
                            if let Some(approval) = session.approvals.get_mut(&approved_approval_id)
                            {
                                approval.status = ApprovalStatus::Consumed;
                            }
                            session.active_approval_id = None;
                            self.sessions.save(&session).await?;
                        }

                        let after_tool_messages = session.messages.clone();
                        let after_tool = self
                            .run_hook_checked(
                                HookPoint::AfterToolResult,
                                &mut session,
                                &session_id,
                                &invocation,
                                &continuation.model,
                                &after_tool_messages,
                                current_candidate.as_ref(),
                                Some(&call),
                                Some(&result),
                                None,
                                request_id,
                                trace_id,
                                &module_state,
                            )
                            .await;
                        let after_tool = match after_tool {
                            Ok(effects) => effects,
                            Err(error) => {
                                Self::append_skipped_tool_results(
                                    &mut session,
                                    &continuation.tool_calls,
                                    index.saturating_add(1),
                                    "module hook failed after tool result",
                                    clock,
                                );
                                self.sessions.save(&session).await?;
                                return Err(error);
                            }
                        };
                        next_overlays.extend(after_tool.prompt_overlays);

                        match after_tool.directive {
                            ModuleDirective::Continue => {}
                            ModuleDirective::Retry { feedback } => {
                                Self::append_skipped_tool_results(
                                    &mut session,
                                    &continuation.tool_calls,
                                    index.saturating_add(1),
                                    "remaining tool calls skipped by module after result",
                                    clock,
                                );
                                retry_requested = Some(feedback);
                                break;
                            }
                            ModuleDirective::Stop { reason } => {
                                Self::append_skipped_tool_results(
                                    &mut session,
                                    &continuation.tool_calls,
                                    index.saturating_add(1),
                                    "remaining tool calls stopped by module after result",
                                    clock,
                                );
                                return self
                                    .fail_module_stop(
                                        &mut session,
                                        request_id,
                                        trace_id,
                                        after_tool
                                            .directive_module_id
                                            .as_deref()
                                            .unwrap_or("unknown"),
                                        reason,
                                    )
                                    .await;
                            }
                        }
                    }
                    ToolDispatch::Pending {
                        capability_id,
                        tool_name,
                        tool_call,
                        effective_invocation,
                        governance_hook,
                        governance_reason,
                    } => {
                        let approval_id = ApprovalId::new();
                        let created_at = Timestamp::from_clock(clock);
                        let expires_at = Timestamp::from_epoch_millis(
                            created_at
                                .epoch_millis()
                                .saturating_add(self.config.approval_ttl_ms as i64),
                        )
                        .unwrap_or(created_at);
                        let fingerprint = operation_fingerprint_with_invocation(
                            "capability_dispatch",
                            &capability_id,
                            &tool_name,
                            &tool_call.id,
                            &tool_call.arguments,
                            effective_invocation.as_ref().map(|frozen| &frozen.payload),
                            session_id,
                            request_id,
                            continuation.round,
                        );
                        let frozen = FrozenTurnContinuation {
                            request_id,
                            trace_id,
                            model: continuation.model.clone(),
                            round: continuation.round,
                            tool_calls: continuation.tool_calls.clone(),
                            next_tool_index: index,
                            approved_tool_index: None,
                            approved_approval_id: None,
                            module_invocations: module_state.used(),
                        };
                        let pending = PendingApproval {
                            approval_id,
                            session_id,
                            request_id,
                            trace_id,
                            round: continuation.round,
                            capability_id,
                            tool_name,
                            tool_call,
                            effective_invocation,
                            governance_hook: governance_hook.clone(),
                            governance_reason,
                            operation_fingerprint: fingerprint,
                            created_at,
                            expires_at,
                            status: ApprovalStatus::Pending,
                            continuation: frozen,
                            human_reason: None,
                        };

                        session.record(
                            request_id,
                            trace_id,
                            SessionEventKind::ApprovalRequired {
                                hook: governance_hook,
                                action: "capability_dispatch".into(),
                                reason: pending.governance_reason.clone(),
                                round: continuation.round,
                                approval_id,
                            },
                            clock,
                        );
                        trace.record(
                            Timestamp::from_clock(clock),
                            TraceEvent::ApprovalRequested {
                                approval_id,
                                capability: pending.capability_id.clone(),
                                tool_call_id: pending.tool_call.id.clone(),
                                round: continuation.round,
                            },
                        );

                        session.approvals.insert(approval_id, pending.clone());
                        session.active_approval_id = Some(approval_id);
                        self.sessions.save(&session).await?;

                        return Ok(TurnOutcome::PendingApproval(PendingApprovalView::from(
                            &pending,
                        )));
                    }
                }

                continuation.next_tool_index += 1;
                continuation.approved_tool_index = None;
                continuation.approved_approval_id = None;
            }

            if let Some(feedback) = retry_requested {
                retry_scaffolding = vec![NormalizedMessage::user(feedback)];
                pending_overlays = next_overlays;
                self.sessions.save(&session).await?;
                continuation.round += 1;
                continuation.tool_calls.clear();
                continuation.next_tool_index = 0;
                continuation.approved_tool_index = None;
                continuation.approved_approval_id = None;
                continue;
            }

            // Every tool call in this round has a result in the transcript.
            self.sessions.save(&session).await?;

            pending_overlays = next_overlays;
            continuation.round += 1;
            continuation.tool_calls.clear();
            continuation.next_tool_index = 0;
            continuation.approved_tool_index = None;
            continuation.approved_approval_id = None;
        }
    }

    async fn run_hook_checked(
        &self,
        hook: HookPoint,
        session: &mut Session,
        session_id: &SessionId,
        invocation: &InvocationContext,
        model: &str,
        messages: &[NormalizedMessage],
        candidate: Option<&NormalizedResponse>,
        tool_call: Option<&ToolCall>,
        tool_result: Option<&ToolResult>,
        error: Option<&str>,
        request_id: RequestId,
        trace_id: TraceId,
        module_state: &Arc<ModuleTurnState>,
    ) -> RuntimeResult<HookEffects> {
        match self
            .run_hook(
                hook,
                session_id,
                trace_id,
                invocation,
                model,
                messages,
                candidate,
                tool_call,
                tool_result,
                error,
                module_state,
            )
            .await
        {
            Ok(effects) => Ok(effects),
            Err(runtime_error) => {
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::ExecutionFailed {
                        phase: "module_hook".into(),
                        error: runtime_error.to_string(),
                    },
                    self.clock.as_ref(),
                );
                self.sessions.save(session).await?;
                Err(runtime_error)
            }
        }
    }

    async fn run_hook(
        &self,
        hook: HookPoint,
        session_id: &SessionId,
        trace_id: TraceId,
        invocation: &InvocationContext,
        model: &str,
        messages: &[NormalizedMessage],
        candidate: Option<&NormalizedResponse>,
        tool_call: Option<&ToolCall>,
        tool_result: Option<&ToolResult>,
        error: Option<&str>,
        module_state: &Arc<ModuleTurnState>,
    ) -> RuntimeResult<HookEffects> {
        let mut effects = HookEffects::default();
        let active_tools = self.tools();
        for module in &self.modules {
            let manifest = module.manifest();
            // One turn-scoped spawner per hook: the borrowed accessor and the
            // owned handle below dereference the same Arc allocation, so every
            // path shares this turn's budget and identity.
            let spawner = Arc::new(RuntimeSubLoopSpawner::new(
                self.providers_arc(),
                active_tools.clone(),
                Arc::clone(&self.governance),
                *session_id,
                trace_id,
                model,
                manifest.id.as_str(),
                Arc::clone(module_state),
                invocation.depth,
            ));
            let invoker_handle: Arc<dyn ModuleInvoker> = spawner.clone();
            let context = ModuleContext {
                session_id,
                model,
                messages,
                candidate,
                tool_call,
                tool_result,
                invocation,
                module_id: &manifest.id,
                error,
                invoker: &*spawner,
                invoker_handle,
                subloop: &*spawner,
            };
            let outcome =
                module
                    .on_hook(hook, &context)
                    .await
                    .map_err(|source| RuntimeError::Module {
                        module_id: manifest.id.clone(),
                        source,
                    })?;
            effects.push(&manifest.id, outcome);
        }
        Ok(effects)
    }

    /// Close the current assistant tool-call batch when a module prevents the
    /// remaining calls from being dispatched. A tool result is required for
    /// every call in the assistant message before that transcript can be sent
    /// to another provider. The module's retry feedback remains a separate
    /// transient user scaffold; it is never encoded as a tool result.
    fn append_skipped_tool_results(
        session: &mut Session,
        tool_calls: &[ToolCall],
        first_skipped: usize,
        reason: &str,
        clock: &dyn apeireth_core::kernel::Clock,
    ) {
        for call in tool_calls.iter().skip(first_skipped) {
            session.append(
                ToolResult::permanent_error(&call.id, reason)
                    .with_name(&call.name)
                    .into_message(),
                clock,
            );
        }
    }

    /// A claimed approval whose dispatch is vetoed by a module cannot remain
    /// active: the turn will continue with synthetic tool errors (Retry) or
    /// terminate (Stop), so there is no longer an executable approval to resume.
    /// `Interrupted` is the existing fail-closed terminal state for that case.
    fn interrupt_claimed_approval(
        &self,
        session: &mut Session,
        approval_id: ApprovalId,
        request_id: RequestId,
        trace_id: TraceId,
        round: u32,
        clock: &dyn apeireth_core::kernel::Clock,
    ) {
        let interrupted = match session.approvals.get_mut(&approval_id) {
            Some(approval) if approval.status == ApprovalStatus::Claimed => {
                approval.status = ApprovalStatus::Interrupted;
                true
            }
            _ => false,
        };
        if !interrupted {
            return;
        }
        if session.active_approval_id == Some(approval_id) {
            session.active_approval_id = None;
        }
        session.record(
            request_id,
            trace_id,
            SessionEventKind::ApprovalResolved {
                approval_id,
                decision: "interrupted".into(),
                round,
                human_reason: None,
            },
            clock,
        );
    }

    async fn observe_error(
        &self,
        request: &TurnRequest,
        module_state: Arc<ModuleTurnState>,
        error: &RuntimeError,
    ) {
        let messages = match self.sessions.load(&request.session).await {
            Ok(Some(session)) => session.messages,
            _ => Vec::new(),
        };
        let model = request
            .model
            .clone()
            .or_else(|| self.config.default_model.clone())
            .unwrap_or_default();
        let invocation = InvocationContext::user_turn();
        let error_text = error.to_string();
        // Error observation is deliberately best-effort. A failing error hook
        // cannot replace, swallow, or rewrite the original runtime failure.
        let _ = self
            .run_hook(
                HookPoint::OnError,
                &request.session,
                TraceId::new(),
                &invocation,
                &model,
                &messages,
                None,
                None,
                None,
                Some(&error_text),
                &module_state,
            )
            .await;
    }

    async fn fail_module_stop(
        &self,
        session: &mut Session,
        request_id: RequestId,
        trace_id: TraceId,
        module_id: &str,
        reason: String,
    ) -> RuntimeResult<TurnOutcome> {
        let error = RuntimeError::ModuleStopped {
            module_id: module_id.to_string(),
            reason,
        };
        session.record(
            request_id,
            trace_id,
            SessionEventKind::ExecutionFailed {
                phase: "module_stop".into(),
                error: error.to_string(),
            },
            self.clock.as_ref(),
        );
        self.sessions.save(session).await?;
        Err(error)
    }

    /// Expire a pending approval that has outlived its TTL so a later turn can start.
    ///
    /// Claimed approvals are not expired here: their effect may already have
    /// started, and automatic retry is unsafe.
    async fn expire_active_approval_if_needed(&self, session: &mut Session) -> RuntimeResult<()> {
        let Some(approval_id) = session.active_approval_id else {
            return Ok(());
        };
        let Some(approval) = session.approvals.get(&approval_id).cloned() else {
            session.active_approval_id = None;
            self.sessions.save(session).await?;
            return Ok(());
        };
        if approval.status != ApprovalStatus::Pending {
            return Ok(());
        }
        let now = Timestamp::from_clock(self.clock.as_ref());
        if !approval.is_expired(now) {
            return Ok(());
        }

        let expired = {
            let mut expired = approval.clone();
            expired.status = ApprovalStatus::Expired;
            expired.human_reason = None;
            expired
        };
        session.approvals.insert(approval_id, expired);
        session.active_approval_id = None;
        session.record(
            approval.request_id,
            approval.trace_id,
            SessionEventKind::ApprovalResolved {
                approval_id,
                decision: "expired".into(),
                round: approval.round,
                human_reason: None,
            },
            self.clock.as_ref(),
        );
        Self::append_skipped_tool_results(
            session,
            &approval.continuation.tool_calls,
            approval.continuation.next_tool_index,
            "operation expired before tool dispatch",
            self.clock.as_ref(),
        );
        self.sessions.save(session).await?;
        Ok(())
    }

    /// Ask governance whether this round's completion may proceed.
    async fn authorize_completion(
        &self,
        trace: &mut ExecutionTrace,
        session_id: &SessionId,
        request_id: RequestId,
        trace_id: TraceId,
        model: &str,
        session: &mut Session,
        round: u32,
    ) -> RuntimeResult<()> {
        let action = Action::Completion {
            model,
            message_count: session.len(),
        };
        let label = action.label();
        let verdict = self
            .governance
            .evaluate_verbose(&GovernanceRequest::new(
                action,
                *session_id,
                trace_id,
                round,
            ))
            .await;
        let hook = verdict.hook;
        let owner = verdict.owner;
        let decision = verdict.decision;

        trace.record(
            Timestamp::from_clock(self.clock.as_ref()),
            TraceEvent::GovernanceEvaluated {
                hook: hook.clone(),
                owner,
                action: label.to_string(),
                decision: decision.label().to_string(),
                reason: decision.reason().map(str::to_owned),
                round,
            },
        );

        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny { reason } => {
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::GovernanceDenied {
                        hook: hook.clone(),
                        action: label.to_string(),
                        reason: reason.clone(),
                        round,
                    },
                    self.clock.as_ref(),
                );
                Err(RuntimeError::Denied { hook, reason })
            }
            Decision::RequireApproval { reason } => {
                // Completion-level approval is not resumable in this phase.
                // It must not mint a stable ApprovalId that pretends a pending
                // approval entity exists.
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::CompletionApprovalRequired {
                        hook: hook.clone(),
                        action: label.to_string(),
                        reason: reason.clone(),
                        round,
                    },
                    self.clock.as_ref(),
                );
                Err(RuntimeError::ApprovalRequired {
                    hook,
                    reason,
                    approval: None,
                    session: Some(*session_id),
                })
            }
        }
    }

    /// Resolve, authorize, and run one tool call.
    ///
    /// When `preapproved` is true the tool has already passed human approval
    /// and must be dispatched without a second governance evaluation. The
    /// approved dispatch must execute the exact stored frozen invocation.
    async fn dispatch_one_tool(
        &self,
        trace: &mut ExecutionTrace,
        session: &mut Session,
        session_id: &SessionId,
        request_id: RequestId,
        trace_id: TraceId,
        call: &ToolCall,
        round: u32,
        preapproved: bool,
        approved_approval: Option<&PendingApproval>,
    ) -> RuntimeResult<ToolDispatch> {
        let clock = self.clock.as_ref();

        let module_tool = self.capabilities.find_by_name(&call.name);
        let plugin_tool = self.plugins.tool_by_name(&call.name);
        let Some(tool) = (match (module_tool, plugin_tool) {
            (Some(tool), None) | (None, Some(tool)) => Some(tool),
            (None, None) => None,
            (Some(_), Some(_)) => None,
        }) else {
            let available = self
                .tool_declarations()
                .iter()
                .map(|t| t.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let reason = if available.is_empty() {
                format!("no tool named {:?}; no tools are available", call.name)
            } else {
                format!(
                    "no tool named {:?}; available tools: {available}",
                    call.name
                )
            };
            trace.record(
                Timestamp::from_clock(clock),
                TraceEvent::CapabilityUnavailable {
                    requested: call.name.clone(),
                    tool_call_id: call.id.clone(),
                    reason: reason.clone(),
                    round,
                },
            );
            session.record(
                request_id,
                trace_id,
                SessionEventKind::ToolFailed {
                    capability: None,
                    tool_call_id: call.id.clone(),
                    error: reason.clone(),
                    round,
                },
                clock,
            );
            return Ok(ToolDispatch::Result(
                ToolResult::permanent_error(&call.id, reason).with_name(&call.name),
            ));
        };

        let capability = tool.id().clone();

        if preapproved {
            let Some(approval) = approved_approval else {
                let reason = "approved dispatch is missing its frozen approval".to_string();
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::ToolFailed {
                        capability: Some(capability.clone()),
                        tool_call_id: call.id.clone(),
                        error: reason.clone(),
                        round,
                    },
                    clock,
                );
                return Ok(ToolDispatch::Result(
                    ToolResult::permanent_error(&call.id, reason).with_name(&call.name),
                ));
            };

            if let Err(reason) = self.verify_frozen_approval_binding(approval, call, tool.as_ref())
            {
                // Fail closed: the stored approval does not match the live
                // capability or its fingerprint. Do not invoke.
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::ToolFailed {
                        capability: Some(capability.clone()),
                        tool_call_id: call.id.clone(),
                        error: reason.clone(),
                        round,
                    },
                    clock,
                );
                return Ok(ToolDispatch::Result(
                    ToolResult::permanent_error(&call.id, reason).with_name(&call.name),
                ));
            }

            trace.record(
                Timestamp::from_clock(clock),
                TraceEvent::CapabilityDispatched {
                    capability: capability.clone(),
                    tool_call_id: call.id.clone(),
                    round,
                },
            );

            let result = tool
                .invoke_frozen(call, approval.effective_invocation.as_ref())
                .await;

            if !result.is_ok() {
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::ToolFailed {
                        capability: Some(capability.clone()),
                        tool_call_id: call.id.clone(),
                        error: result.render(),
                        round,
                    },
                    clock,
                );
            }

            trace.record(
                Timestamp::from_clock(clock),
                TraceEvent::CapabilityCompleted {
                    capability,
                    tool_call_id: call.id.clone(),
                    succeeded: result.is_ok(),
                    round,
                },
            );

            return Ok(ToolDispatch::Result(result));
        }

        let action = Action::CapabilityDispatch {
            capability: &capability,
            arguments: &call.arguments,
        };
        let label = action.label();
        let verdict = self
            .governance
            .evaluate_verbose(
                &GovernanceRequest::new(action, *session_id, trace_id, round)
                    .with_action_id(&call.id),
            )
            .await;
        let hook = verdict.hook;
        let owner = verdict.owner;
        let decision = verdict.decision;

        trace.record(
            Timestamp::from_clock(clock),
            TraceEvent::GovernanceEvaluated {
                hook: hook.clone(),
                owner,
                action: label.to_string(),
                decision: decision.label().to_string(),
                reason: decision.reason().map(str::to_owned),
                round,
            },
        );

        match decision {
            Decision::Allow => {}
            Decision::Deny { reason } => {
                session.record(
                    request_id,
                    trace_id,
                    SessionEventKind::GovernanceDenied {
                        hook,
                        action: label.to_string(),
                        reason: reason.clone(),
                        round,
                    },
                    clock,
                );
                return Ok(ToolDispatch::Result(
                    ToolResult::permanent_error(
                        &call.id,
                        format!("refused by governance: {reason}"),
                    )
                    .with_name(&call.name),
                ));
            }
            Decision::RequireApproval { reason } => {
                let effective_invocation = match tool.freeze_invocation(call) {
                    Ok(frozen) => frozen,
                    Err(result) => {
                        // A tool whose effective invocation cannot be prepared
                        // must not produce a pending approval: asking a human to
                        // approve an invalid operation is misleading.
                        session.record(
                            request_id,
                            trace_id,
                            SessionEventKind::ToolFailed {
                                capability: Some(capability.clone()),
                                tool_call_id: call.id.clone(),
                                error: result.render(),
                                round,
                            },
                            clock,
                        );
                        return Ok(ToolDispatch::Result(result));
                    }
                };

                return Ok(ToolDispatch::Pending {
                    capability_id: capability,
                    tool_name: call.name.clone(),
                    tool_call: call.clone(),
                    effective_invocation,
                    governance_hook: hook,
                    governance_reason: reason,
                });
            }
        }

        trace.record(
            Timestamp::from_clock(clock),
            TraceEvent::CapabilityDispatched {
                capability: capability.clone(),
                tool_call_id: call.id.clone(),
                round,
            },
        );

        let result = tool.invoke(call).await;

        if !result.is_ok() {
            session.record(
                request_id,
                trace_id,
                SessionEventKind::ToolFailed {
                    capability: Some(capability.clone()),
                    tool_call_id: call.id.clone(),
                    error: result.render(),
                    round,
                },
                clock,
            );
        }

        trace.record(
            Timestamp::from_clock(clock),
            TraceEvent::CapabilityCompleted {
                capability,
                tool_call_id: call.id.clone(),
                succeeded: result.is_ok(),
                round,
            },
        );

        Ok(ToolDispatch::Result(result))
    }

    /// Recompute the operation fingerprint from the stored approval and verify
    /// the live capability identity matches the frozen approval.
    fn verify_frozen_approval_binding(
        &self,
        approval: &PendingApproval,
        call: &ToolCall,
        tool: &dyn apeireth_plugin::ToolCapability,
    ) -> Result<(), String> {
        if tool.id() != &approval.capability_id {
            return Err(format!(
                "capability mismatch: approval is for {} but the tool named {:?} resolves to {}",
                approval.capability_id,
                call.name,
                tool.id()
            ));
        }
        if call.name != approval.tool_name {
            return Err(format!(
                "tool name mismatch: approval is for {:?} but the call names {:?}",
                approval.tool_name, call.name
            ));
        }
        if call.id != approval.tool_call.id {
            return Err(format!(
                "tool call id mismatch: approval is for {:?} but the call id is {:?}",
                approval.tool_call.id, call.id
            ));
        }

        let computed = operation_fingerprint_with_invocation(
            "capability_dispatch",
            &approval.capability_id,
            &approval.tool_name,
            &approval.tool_call.id,
            &approval.tool_call.arguments,
            approval
                .effective_invocation
                .as_ref()
                .map(|frozen| &frozen.payload),
            approval.session_id,
            approval.request_id,
            approval.round,
        );
        if computed != approval.operation_fingerprint {
            return Err("frozen invocation fingerprint mismatch".into());
        }

        Ok(())
    }

    fn emit_outcome_events(
        &self,
        session: SessionId,
        request: RequestId,
        trace: TraceId,
        result: &RuntimeResult<TurnOutcome>,
    ) {
        match result {
            Ok(TurnOutcome::Completed(response)) => {
                for entry in &response.trace.entries {
                    self.emit_event(RuntimeEvent::Trace {
                        session: response.session,
                        trace: response.trace.trace,
                        at: entry.at,
                        event: entry.event.clone(),
                    });
                }
                self.emit_event(RuntimeEvent::TurnCompleted {
                    session: response.session,
                    request: response.request,
                    trace: response.trace.trace,
                    rounds: response.rounds,
                    served_by: response.served_by.clone(),
                });
            }
            Ok(TurnOutcome::PendingApproval(view)) => {
                self.emit_event(RuntimeEvent::ApprovalRequired {
                    session: view.session_id,
                    request: view.request_id,
                    trace: view.trace_id,
                    approval: view.approval_id,
                    capability: view.capability_id.clone(),
                    tool_name: view.tool_name.clone(),
                    tool_call_id: view.tool_call.id.clone(),
                });
            }
            Err(error) => self.emit_event(RuntimeEvent::TurnFailed {
                session,
                request,
                trace,
                error: error.to_string(),
            }),
        }
    }

    /// Record the assistant's answer, persist the session, and close the trace.
    async fn finish_turn(
        &self,
        session: &mut Session,
        mut trace: ExecutionTrace,
        request_id: RequestId,
        served_by: CapabilityId,
        response: NormalizedResponse,
        rounds: u32,
    ) -> RuntimeResult<TurnResponse> {
        let clock = self.clock.as_ref();
        session.append(
            NormalizedMessage::assistant(response.content.clone()),
            clock,
        );
        session.record(
            request_id,
            trace.trace,
            SessionEventKind::TurnCompleted { rounds },
            clock,
        );
        self.sessions.save(&session).await?;

        trace.record(
            Timestamp::from_clock(clock),
            TraceEvent::TurnCompleted { rounds },
        );

        Ok(TurnResponse {
            session: session.id,
            request: request_id,
            text: response.content,
            served_by,
            usage: response.usage,
            rounds,
            trace,
        })
    }
}
