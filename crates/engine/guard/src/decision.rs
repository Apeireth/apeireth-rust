//! Guard Decision & Evidence representation.

use apeireth_core::kernel::Metadata;
use apeireth_governance::{Decision, GovernanceVerdict};
use serde::{Deserialize, Serialize};

use crate::classifier::RiskPrediction;

/// The stage that finalized the guard decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardStage {
    FastGuard,
    ChainGuard,
    DecisionFusion,
}

/// Structured, machine-readable outcome from the two-stage safety classifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardDecision {
    /// Canonical governance decision.
    pub decision: Decision,
    /// Assessed risk score in [0.0, 1.0].
    pub risk_score: f64,
    /// Specific risk classification categories or reasons.
    pub reasons: Vec<String>,
    /// Concrete machine-readable evidence items.
    pub evidence: Vec<String>,
    /// The stage that finalized the decision.
    pub stage: GuardStage,
    /// Optional local model output used by decision fusion.
    #[serde(default)]
    pub classifier_prediction: Option<RiskPrediction>,
}

impl GuardDecision {
    /// Allow immediately from Fast Guard.
    pub fn allow_fast() -> Self {
        Self {
            decision: Decision::Allow,
            risk_score: 0.0,
            reasons: Vec::new(),
            evidence: Vec::new(),
            stage: GuardStage::FastGuard,
            classifier_prediction: None,
        }
    }

    /// Whether the action is permitted without approval.
    pub fn is_allowed(&self) -> bool {
        self.decision.is_allowed()
    }

    /// Convert into a canonical [`GovernanceVerdict`].
    pub fn to_verdict(&self, hook_name: impl Into<String>) -> GovernanceVerdict {
        let mut metadata = Metadata::new();
        metadata.insert(
            "guard_risk_score".to_string(),
            format!("{:.2}", self.risk_score),
        );
        metadata.insert(
            "guard_stage".to_string(),
            match self.stage {
                GuardStage::FastGuard => "fast_guard".to_string(),
                GuardStage::ChainGuard => "chain_guard".to_string(),
                GuardStage::DecisionFusion => "decision_fusion".to_string(),
            },
        );
        if !self.reasons.is_empty() {
            metadata.insert("guard_reasons".to_string(), self.reasons.join(", "));
        }
        if !self.evidence.is_empty() {
            metadata.insert("guard_evidence".to_string(), self.evidence.join("; "));
        }
        if let Some(prediction) = &self.classifier_prediction {
            metadata.insert(
                "guard_classifier_available".to_string(),
                prediction.available.to_string(),
            );
            metadata.insert(
                "guard_classifier_model".to_string(),
                prediction.model_version.clone(),
            );
            metadata.insert(
                "guard_classifier_class".to_string(),
                serde_json::to_string(&prediction.class)
                    .unwrap_or_else(|_| "unavailable".to_string())
                    .trim_matches('"')
                    .to_string(),
            );
        }

        GovernanceVerdict {
            hook: hook_name.into(),
            owner: None,
            decision: self.decision.clone(),
            metadata,
        }
    }

    /// Convert into a JSON object for telemetry and gateway introspection.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "decision": self.decision,
            "risk_score": self.risk_score,
            "reasons": self.reasons,
            "evidence": self.evidence,
            "stage": self.stage,
            "classifier_prediction": self.classifier_prediction,
        })
    }
}
