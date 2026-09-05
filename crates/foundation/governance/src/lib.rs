//! Canonical governance: the decision point the runtime consults before it acts.
//!
//! # The contract
//!
//! One trait, [`GovernanceHook`], answering one question: *may this action
//! proceed?* The answer is a [`Decision`], and the runtime is required to honour
//! it. That is the entire canonical surface.
//!
//! # What this crate is not
//!
//! It is not a second approval authority, council loop, or sovereignty runtime.
//! Recovered donor algorithms (Colang parser, approval-policy scoring, eval
//! stats, evidence checker, hold/synthesis rubric, risk-rank / fail-closed)
//! live here as **default-off library helpers**. They do not implement
//! [`GovernanceHook`] and are not installed in [`GovernancePipeline`]. The
//! runtime still consults one hook; mapping a helper onto [`Decision`] is the
//! caller's job.
//!
//! Keeping the contract this thin is deliberate. The branch this converges had
//! governance logic scattered across several crates with no shared decision type,
//! so the runtime could not consult "governance" at all — only specific gates it
//! happened to know about, in an order fixed by whoever wrote the call site. A
//! single hook the runtime can hold as `Arc<dyn GovernanceHook>` inverts that: the
//! runtime asks, policy answers, and neither needs to know the other's internals.
//!
//! # Composition
//!
//! [`GovernancePipeline`] runs hooks in order and stops at the first non-allow
//! decision. This is the "five gates in sequence" arrangement from the nested
//! `reconstruction_v2` prototype, generalized so gates are values rather than
//! hardcoded call sites.
//!
//! # Layering
//!
//! Depends on `apeireth-core` and nothing else. Must not depend on the runtime,
//! the gateway, storage, or any concrete capability: a policy that knows what it
//! is guarding cannot be reused to guard anything else.

#![deny(unsafe_code)]

use std::fmt;
use std::sync::Arc;

use apeireth_core::kernel::{CapabilityId, Metadata, PluginId, SessionId, TraceId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod approval_policy;
pub mod audit;
pub mod colang;
pub mod eval;
pub mod evidence;
pub mod input_security;
pub mod permission;
pub mod rate_limit;
// B5 · Phase 4 (research, 默认关闭): 校准门控自治 (RA-4 风险优先阶梯 + hysteresis + shadow).
pub mod research_autonomy;
pub use research_autonomy::{
    research_fixed_threshold, ResearchAutonomyDiagnostics, ResearchAutonomyGovernor,
    ResearchAutonomyState, ResearchAutonomyThresholds, ResearchRiskFirstLadder,
    ResearchShadowAutonomy, ResearchShadowDivergence, ResearchStrengthTier,
};
pub mod risk;
pub mod rubric;
pub mod tool_desc_audit;
pub mod untrusted_mark;

pub use audit::{AuditChainError, AuditHashChain, AuditRecord, GENESIS_PREVIOUS_HASH};
pub use input_security::{
    CredentialDisclosureHook, PiiDetector, PiiFinding, PiiKind, PromptInjectionHeuristic,
    PromptInjectionHook, PromptInjectionKind, PromptInjectionSignal,
};
pub use permission::{Permission, PermissionGovernanceHook, PermissionPolicy, PermissionSet};
pub use rate_limit::{RateLimitConfig, RateLimitGovernanceHook, TrustTier};
pub use tool_desc_audit::{
    AuditSeverity, ToolDescAuditError, ToolDescAuditResult, ToolDescAuditor,
};
pub use untrusted_mark::{
    UntrustedContentPayload, UntrustedContentWrapper, UNTRUSTED_TAG_CLOSE, UNTRUSTED_TAG_OPEN,
};

pub use approval_policy::{
    best_approval_match, extract_commands, frequency_count, is_high_risk, parse_approval_entry,
    ApprovalPolicyEngine, CallRecord, ParsedApprovalEntry, PolicyMatch, APPROVAL_TIMEOUT_MS,
    DEFAULT_HIGH_RISK_PREFIXES, FREQUENCY_MAX_CALLS, FREQUENCY_WINDOW_MS, SILENT_REJECT_SUFFIX,
};
pub use colang::{
    extract_action_name, ColangDefine, ColangDslGuard, ColangElement, ColangElementKind,
    ColangGuardConfig, ColangGuardOutcome, ColangParseError, ColangParser, ColangValidationError,
    ColangValidationReport, ColangValidator, DslOnionLayer, DslOnionVerdict, ParsedColangFile,
};
pub use eval::{
    is_valid_percentile, mean, percentile, stddev, weighted_mean, CategoryBreakdown, EvalScore,
    TaskResult, TaskSummary,
};
pub use evidence::{
    EvidenceCheck, EvidenceEntry, EvidenceGuard, EvidenceKind, INFERENCE_CONFIDENCE_CEILING,
};
pub use risk::{
    check_no_degrade, is_degrade, risk_rank, run_fail_closed, ApplyPhase, FailClosedError,
    FailClosedPhase, NoDegradeCheck, PreparePhase, RegressionAssertion, VerifyPhase,
};
pub use rubric::{
    passes_strategy, synthesize, AdvisorDomain, Ballot, HoldDecision, HoldThreshold, HoldTrigger,
    StanceKind, SynthesisReport, VotingStrategy, HOLD_DELIBERATION_TIMEOUT_MS,
    HOLD_STRONG_DISAPPROVE_PERCENT, SUPERMAJORITY_FRACTION,
};

/// What the runtime is about to do.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Action<'a> {
    /// Send a request to a provider.
    Completion {
        /// The model that will serve it.
        model: &'a str,
        /// How many messages the transcript currently holds.
        message_count: usize,
    },
    /// Dispatch a tool call the model asked for.
    ///
    /// Carries the arguments, because whether a shell call is acceptable depends
    /// entirely on what it would run.
    CapabilityDispatch {
        /// The capability to be invoked.
        capability: &'a CapabilityId,
        /// The arguments the model produced.
        arguments: &'a serde_json::Value,
    },
}

impl Action<'_> {
    /// A short stable label for logs and audit records.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Completion { .. } => "completion",
            Self::CapabilityDispatch { .. } => "capability_dispatch",
        }
    }
}

/// One action, with the context needed to judge it.
#[derive(Debug, Clone)]
pub struct GovernanceRequest<'a> {
    /// The action under consideration.
    pub action: Action<'a>,
    /// The session it belongs to.
    pub session: SessionId,
    /// The turn it belongs to, so a decision can be correlated with its cause.
    pub trace: TraceId,
    /// Which round of the agent loop this is, counting from 1.
    ///
    /// A hook can use this to bound runaway tool loops, which is the most common
    /// reason a turn needs stopping without anything being individually unsafe.
    pub round: u32,
    /// Optional caller-supplied identity for the concrete action being
    /// evaluated (normally the provider/tool call id).  Completion requests
    /// may leave this unset; hooks then derive a deterministic identity.
    pub action_id: Option<&'a str>,
}

impl<'a> GovernanceRequest<'a> {
    /// Build a request.
    pub const fn new(action: Action<'a>, session: SessionId, trace: TraceId, round: u32) -> Self {
        Self {
            action,
            session,
            trace,
            round,
            action_id: None,
        }
    }

    /// Bind the governance request to the concrete action identity known by
    /// the runtime at dispatch time.
    #[must_use]
    pub const fn with_action_id(mut self, action_id: &'a str) -> Self {
        self.action_id = Some(action_id);
        self
    }
}

/// The verdict on one action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Proceed.
    Allow,
    /// Do not proceed.
    Deny {
        /// Why. This reaches the model as a tool error, so it should be
        /// intelligible to a reader who cannot see the policy.
        reason: String,
    },
    /// Do not proceed without a human decision.
    ///
    /// Distinct from [`Decision::Deny`] because the correct runtime behaviour
    /// differs: a denial is final and the turn continues without the action,
    /// whereas an approval requirement means the turn should suspend. Collapsing
    /// the two forces the runtime to guess which one it is looking at.
    RequireApproval {
        /// What a human is being asked to approve.
        reason: String,
    },
}

impl Decision {
    /// Deny with a reason.
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    /// Require human approval, with a reason.
    pub fn require_approval(reason: impl Into<String>) -> Self {
        Self::RequireApproval {
            reason: reason.into(),
        }
    }

    /// Whether the action may proceed unconditionally.
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// The stated reason, when the action was not allowed.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Allow => None,
            Self::Deny { reason } | Self::RequireApproval { reason } => Some(reason),
        }
    }

    /// Stable decision label for structured traces.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny { .. } => "deny",
            Self::RequireApproval { .. } => "require_approval",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allow => f.write_str("allow"),
            Self::Deny { reason } => write!(f, "deny: {reason}"),
            Self::RequireApproval { reason } => write!(f, "require approval: {reason}"),
        }
    }
}

/// A decision, plus which hook made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceVerdict {
    /// The hook that produced this decision.
    pub hook: String,
    /// Plugin that owns the hook, when the hook comes from a plugin.
    pub owner: Option<PluginId>,
    /// What it decided.
    pub decision: Decision,
    /// Additional annotations from the hook.
    pub metadata: Metadata,
}

impl GovernanceVerdict {
    /// A verdict from a named hook.
    pub fn new(hook: impl Into<String>, decision: Decision) -> Self {
        Self {
            hook: hook.into(),
            owner: None,
            decision,
            metadata: Metadata::new(),
        }
    }

    /// Attribute the verdict to a plugin owner.
    #[must_use]
    pub fn with_owner(mut self, owner: Option<PluginId>) -> Self {
        self.owner = owner;
        self
    }

    /// Whether the action may proceed.
    pub const fn is_allowed(&self) -> bool {
        self.decision.is_allowed()
    }
}

/// Decides whether an action may proceed.
#[async_trait]
pub trait GovernanceHook: Send + Sync {
    /// Stable name, used in verdicts and audit records.
    fn name(&self) -> &str;

    /// Plugin owner when this hook is contributed by a plugin.
    fn owner(&self) -> Option<&PluginId> {
        None
    }

    /// Judge one action.
    ///
    /// Returns a [`Decision`] rather than a `Result`: a policy refusal is a
    /// normal, expected outcome, not a malfunction. A hook that genuinely cannot
    /// reach its backing store should fail closed by denying with a reason
    /// saying so, which is both safer and more legible than an error type the
    /// caller has to interpret.
    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision;

    /// Judge one action and preserve the identity of the deciding hook.
    async fn evaluate_verbose(&self, request: &GovernanceRequest<'_>) -> GovernanceVerdict {
        GovernanceVerdict::new(self.name(), self.evaluate(request).await)
            .with_owner(self.owner().cloned())
    }
}

/// Allows everything.
///
/// Explicitly permissive. Tests and embeddings that need an open runtime must
/// install this hook themselves. Production and the default runtime builder
/// fail closed instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

#[async_trait]
impl GovernanceHook for AllowAll {
    fn name(&self) -> &str {
        "allow_all"
    }

    async fn evaluate(&self, _request: &GovernanceRequest<'_>) -> Decision {
        Decision::Allow
    }
}

/// Fail-closed default: completions may proceed, capability dispatch may not.
///
/// A runtime with no policy still answers plain chat. It must not execute a
/// tool, MCP capability, or other side effect until an explicit grant is
/// installed. Completions stay allowed so a zero-module kernel can still talk.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyUnconfigured;

#[async_trait]
impl GovernanceHook for DenyUnconfigured {
    fn name(&self) -> &str {
        "deny_unconfigured"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        match &request.action {
            Action::Completion { .. } => Decision::Allow,
            Action::CapabilityDispatch { capability, .. } => Decision::deny(format!(
                "capability {capability} is not permitted: no governance policy is configured"
            )),
        }
    }
}

/// Denies dispatch of specific capabilities.
///
/// Small, but enough to prove the pipeline actually gates: the runtime's E2E test
/// uses it to show that a denied tool never reaches its plugin.
#[derive(Debug, Clone, Default)]
pub struct DenyCapabilities {
    denied: Vec<CapabilityId>,
}

impl DenyCapabilities {
    /// Deny nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style denial.
    #[must_use]
    pub fn deny(mut self, capability: CapabilityId) -> Self {
        self.denied.push(capability);
        self
    }
}

#[async_trait]
impl GovernanceHook for DenyCapabilities {
    fn name(&self) -> &str {
        "deny_capabilities"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        match &request.action {
            Action::CapabilityDispatch { capability, .. } if self.denied.contains(capability) => {
                Decision::deny(format!("capability {capability} is not permitted"))
            }
            _ => Decision::Allow,
        }
    }
}

/// Stops a turn that keeps calling tools without converging.
#[derive(Debug, Clone, Copy)]
pub struct MaxRounds {
    limit: u32,
}

impl MaxRounds {
    /// Allow at most `limit` rounds.
    pub const fn new(limit: u32) -> Self {
        Self { limit }
    }
}

#[async_trait]
impl GovernanceHook for MaxRounds {
    fn name(&self) -> &str {
        "max_rounds"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        if request.round > self.limit {
            Decision::deny(format!(
                "turn exceeded {} rounds without completing",
                self.limit
            ))
        } else {
            Decision::Allow
        }
    }
}

/// Runs hooks in order and stops at the first non-allow decision.
///
/// Order is significant and preserved: a cheap local check should come before an
/// expensive remote one, and the first refusal short-circuits the rest.
#[derive(Default)]
pub struct GovernancePipeline {
    hooks: Vec<Arc<dyn GovernanceHook>>,
}

impl GovernancePipeline {
    /// An empty pipeline, which allows everything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a hook.
    #[must_use]
    pub fn with(mut self, hook: Arc<dyn GovernanceHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Number of hooks.
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Whether there are no hooks.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Names of the hooks, in evaluation order.
    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(|h| h.name()).collect()
    }
}

#[async_trait]
impl GovernanceHook for GovernancePipeline {
    fn name(&self) -> &str {
        "pipeline"
    }

    async fn evaluate(&self, request: &GovernanceRequest<'_>) -> Decision {
        <Self as GovernanceHook>::evaluate_verbose(self, request)
            .await
            .decision
    }

    async fn evaluate_verbose(&self, request: &GovernanceRequest<'_>) -> GovernanceVerdict {
        for hook in &self.hooks {
            let verdict = hook.evaluate_verbose(request).await;
            if !verdict.is_allowed() {
                return verdict;
            }
        }
        GovernanceVerdict::new(self.name(), Decision::Allow)
    }
}

impl GovernancePipeline {
    /// Evaluate, reporting which hook decided.
    ///
    /// Prefer this over [`GovernanceHook::evaluate`] at a call site that records
    /// an audit trail: "denied" is much less useful than "denied by max_rounds".
    pub async fn evaluate_verbose(&self, request: &GovernanceRequest<'_>) -> GovernanceVerdict {
        <Self as GovernanceHook>::evaluate_verbose(self, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OwnedDeny {
        owner: PluginId,
    }

    #[async_trait]
    impl GovernanceHook for OwnedDeny {
        fn name(&self) -> &str {
            "governance.input.safety"
        }

        fn owner(&self) -> Option<&PluginId> {
            Some(&self.owner)
        }

        async fn evaluate(&self, _request: &GovernanceRequest<'_>) -> Decision {
            Decision::deny("blocked by owned hook")
        }
    }

    fn dispatch_request<'a>(
        capability: &'a CapabilityId,
        arguments: &'a serde_json::Value,
        round: u32,
    ) -> GovernanceRequest<'a> {
        GovernanceRequest::new(
            Action::CapabilityDispatch {
                capability,
                arguments,
            },
            SessionId::new(),
            TraceId::new(),
            round,
        )
    }

    #[tokio::test]
    async fn allow_all_permits_every_action() {
        let cap = CapabilityId::new("tool.shell").unwrap();
        let args = serde_json::json!({ "cmd": "rm -rf /" });
        assert!(AllowAll
            .evaluate(&dispatch_request(&cap, &args, 1))
            .await
            .is_allowed());
    }

    #[tokio::test]
    async fn a_denied_capability_is_refused_with_a_legible_reason() {
        let shell = CapabilityId::new("tool.shell").unwrap();
        let calc = CapabilityId::new("tool.calculator").unwrap();
        let args = serde_json::Value::Null;

        let hook = DenyCapabilities::new().deny(shell.clone());

        let denied = hook.evaluate(&dispatch_request(&shell, &args, 1)).await;
        assert!(!denied.is_allowed());
        assert!(denied.reason().unwrap().contains("tool.shell"), "{denied}");

        assert!(hook
            .evaluate(&dispatch_request(&calc, &args, 1))
            .await
            .is_allowed());
    }

    #[tokio::test]
    async fn a_capability_denial_does_not_block_completions() {
        let shell = CapabilityId::new("tool.shell").unwrap();
        let hook = DenyCapabilities::new().deny(shell);

        let request = GovernanceRequest::new(
            Action::Completion {
                model: "fake-model-1",
                message_count: 2,
            },
            SessionId::new(),
            TraceId::new(),
            1,
        );
        assert!(hook.evaluate(&request).await.is_allowed());
    }

    #[tokio::test]
    async fn max_rounds_stops_a_turn_that_will_not_converge() {
        let cap = CapabilityId::new("tool.calculator").unwrap();
        let args = serde_json::Value::Null;
        let hook = MaxRounds::new(3);

        for round in 1..=3 {
            assert!(
                hook.evaluate(&dispatch_request(&cap, &args, round))
                    .await
                    .is_allowed(),
                "round {round} is within the limit"
            );
        }
        let denied = hook.evaluate(&dispatch_request(&cap, &args, 4)).await;
        assert!(!denied.is_allowed());
        assert!(denied.reason().unwrap().contains('3'), "{denied}");
    }

    #[tokio::test]
    async fn a_pipeline_stops_at_the_first_refusal_and_names_the_hook() {
        let shell = CapabilityId::new("tool.shell").unwrap();
        let args = serde_json::Value::Null;

        let pipeline = GovernancePipeline::new()
            .with(Arc::new(MaxRounds::new(10)))
            .with(Arc::new(DenyCapabilities::new().deny(shell.clone())))
            .with(Arc::new(AllowAll));

        assert_eq!(
            pipeline.hook_names(),
            ["max_rounds", "deny_capabilities", "allow_all"]
        );

        let verdict = pipeline
            .evaluate_verbose(&dispatch_request(&shell, &args, 1))
            .await;
        assert_eq!(verdict.hook, "deny_capabilities");
        assert!(!verdict.is_allowed());
    }

    #[tokio::test]
    async fn a_trait_object_pipeline_preserves_hook_and_plugin_owner() {
        let owner = PluginId::new("plugin.safety").unwrap();
        let pipeline: Arc<dyn GovernanceHook> =
            Arc::new(GovernancePipeline::new().with(Arc::new(OwnedDeny {
                owner: owner.clone(),
            })));
        let capability = CapabilityId::new("tool.shell").unwrap();
        let arguments = serde_json::Value::Null;

        let verdict = pipeline
            .evaluate_verbose(&dispatch_request(&capability, &arguments, 1))
            .await;

        assert_eq!(verdict.hook, "governance.input.safety");
        assert_eq!(verdict.owner, Some(owner));
        assert_eq!(verdict.decision.label(), "deny");
        assert_eq!(verdict.decision.reason(), Some("blocked by owned hook"));
    }

    #[tokio::test]
    async fn an_empty_pipeline_allows() {
        let cap = CapabilityId::new("tool.calculator").unwrap();
        let args = serde_json::Value::Null;
        let pipeline = GovernancePipeline::new();

        assert!(pipeline.is_empty());
        let verdict = pipeline
            .evaluate_verbose(&dispatch_request(&cap, &args, 1))
            .await;
        assert!(verdict.is_allowed());
        assert_eq!(verdict.hook, "pipeline");
    }

    #[tokio::test]
    async fn ordering_decides_which_refusal_is_reported() {
        let shell = CapabilityId::new("tool.shell").unwrap();
        let args = serde_json::Value::Null;

        // Same two hooks, opposite order, same action: round 9 exceeds the
        // round limit *and* the capability is denied.
        let rounds_first = GovernancePipeline::new()
            .with(Arc::new(MaxRounds::new(2)))
            .with(Arc::new(DenyCapabilities::new().deny(shell.clone())));
        let denial_first = GovernancePipeline::new()
            .with(Arc::new(DenyCapabilities::new().deny(shell.clone())))
            .with(Arc::new(MaxRounds::new(2)));

        let req = dispatch_request(&shell, &args, 9);
        assert_eq!(rounds_first.evaluate_verbose(&req).await.hook, "max_rounds");
        assert_eq!(
            denial_first.evaluate_verbose(&req).await.hook,
            "deny_capabilities"
        );
    }

    #[test]
    fn denial_and_approval_are_distinguishable_after_serialization() {
        let deny = Decision::deny("not permitted");
        let approve = Decision::require_approval("needs a human");

        let deny_json = serde_json::to_string(&deny).unwrap();
        let approve_json = serde_json::to_string(&approve).unwrap();
        assert_ne!(deny_json, approve_json);

        assert_eq!(
            serde_json::from_str::<Decision>(&deny_json).unwrap(),
            deny,
            "a runtime must be able to tell a refusal from a pause"
        );
        assert_eq!(
            serde_json::from_str::<Decision>(&approve_json).unwrap(),
            approve
        );
    }

    #[test]
    fn actions_carry_stable_labels() {
        let cap = CapabilityId::new("tool.shell").unwrap();
        let args = serde_json::Value::Null;
        assert_eq!(
            Action::CapabilityDispatch {
                capability: &cap,
                arguments: &args
            }
            .label(),
            "capability_dispatch"
        );
        assert_eq!(
            Action::Completion {
                model: "m",
                message_count: 1
            }
            .label(),
            "completion"
        );
    }
}
