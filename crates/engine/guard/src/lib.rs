//! Two-Stage Agent Behavior-Chain Safety Classifier (`apeireth-guard`).
//!
//! Provides deterministic Fast Guard (Stage A) for immediate sub-millisecond risk rejection,
//! combined with multi-step Behavior Chain Guard (Stage B) for compound risk, escalation,
//! and data egress tracking. Exposes canonical [`apeireth_governance::GovernanceHook`]
//! integration, desensitized ML dataset collection (`guard-dataset-v1`), and Desktop
//! observability endpoints.

pub mod chain;
pub mod chain_guard;
pub mod classifier;
pub mod dataset;
pub mod decision;
pub mod enforcement;
pub mod fast_guard;
pub mod features;
pub mod fusion;
pub mod hook;
pub mod introspection;
pub mod observation;

pub use chain::{ActionNode, ActionStatus, BehaviorChain, BehaviorEdge, BehaviorNode, EdgeType};
pub use chain_guard::ChainGuard;
pub use classifier::{
    ChainRiskClassifier, NoClassifier, RiskClass, RiskPrediction, ThresholdClassifier,
};
pub use dataset::{DatasetRecorder, GuardDatasetRecord};
pub use decision::{GuardDecision, GuardStage};
pub use enforcement::EnforcementDirective;
pub use fast_guard::{FastGuard, FastGuardResult};
pub use features::{AgentChainFeatureV1, AGENT_CHAIN_FEATURE_V1};
pub use fusion::DecisionFusion;
pub use hook::BehaviorChainGuardHook;
pub use introspection::{GuardDryRunRequest, GuardDryRunResponse, GuardEventDto, GuardStatusDto};
pub use observation::{ResourceClass, SafetyObservation, SinkClass, SourceClass};
