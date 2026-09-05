//! Optional local classifier contract for behavior-chain risk.
//!
//! The runtime remains deterministic when no model is installed.  A model is
//! an input to the final fusion step, never a replacement for the Fast Guard
//! or Chain Guard evidence.

use serde::{Deserialize, Serialize};

use crate::features::AgentChainFeatureV1;

/// Coarse risk class emitted by a local classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Low,
    Medium,
    High,
    Critical,
    Unavailable,
}

/// Model output with explicit availability and provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskPrediction {
    pub class: RiskClass,
    pub score: f64,
    pub confidence: f64,
    pub model_version: String,
    pub available: bool,
}

impl RiskPrediction {
    pub fn unavailable() -> Self {
        Self {
            class: RiskClass::Unavailable,
            score: 0.0,
            confidence: 0.0,
            model_version: "none".to_string(),
            available: false,
        }
    }

    pub fn clamped(mut self) -> Self {
        self.score = self.score.clamp(0.0, 1.0);
        self.confidence = self.confidence.clamp(0.0, 1.0);
        self
    }
}

/// Synchronous, allocation-light classifier boundary for the hot path.
pub trait ChainRiskClassifier: Send + Sync {
    fn classify(&self, features: &AgentChainFeatureV1) -> RiskPrediction;

    fn available(&self) -> bool {
        true
    }

    fn model_version(&self) -> Option<String> {
        None
    }
}

/// Default classifier used when no model artifact is configured.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoClassifier;

impl ChainRiskClassifier for NoClassifier {
    fn classify(&self, _features: &AgentChainFeatureV1) -> RiskPrediction {
        RiskPrediction::unavailable()
    }

    fn available(&self) -> bool {
        false
    }
}

/// Small deterministic classifier useful for local integration tests and
/// shadow-mode calibration.  It is deliberately not presented as a trained
/// model.
#[derive(Debug, Clone)]
pub struct ThresholdClassifier {
    pub model_version: String,
}

impl Default for ThresholdClassifier {
    fn default() -> Self {
        Self {
            model_version: "threshold-shadow-v1".to_string(),
        }
    }
}

impl ChainRiskClassifier for ThresholdClassifier {
    fn classify(&self, features: &AgentChainFeatureV1) -> RiskPrediction {
        let score = [
            (features.sensitive_to_external_flow, 0.95),
            (features.retry_after_denial, 0.88),
            (features.alternate_tool_after_denial, 0.82),
            (features.destructive_chain_score(), 0.80),
            (features.credential_access_count > 0, 0.75),
            (features.external_effect_count > 0, 0.55),
        ]
        .into_iter()
        .filter_map(|(matched, value)| matched.then_some(value))
        .max_by(f64::total_cmp)
        .unwrap_or(0.05);
        let class = if score >= 0.90 {
            RiskClass::Critical
        } else if score >= 0.75 {
            RiskClass::High
        } else if score >= 0.40 {
            RiskClass::Medium
        } else {
            RiskClass::Low
        };
        RiskPrediction {
            class,
            score,
            confidence: 0.55,
            model_version: self.model_version.clone(),
            available: true,
        }
    }

    fn model_version(&self) -> Option<String> {
        Some(self.model_version.clone())
    }
}

impl AgentChainFeatureV1 {
    fn destructive_chain_score(&self) -> bool {
        self.delete_count > 0 && self.network_count > 0
    }
}
