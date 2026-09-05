//! Fusion of deterministic guard evidence and optional model output.

use apeireth_governance::Decision;

use crate::classifier::{RiskClass, RiskPrediction};
use crate::decision::{GuardDecision, GuardStage};
use crate::fast_guard::FastGuardResult;
use crate::features::AgentChainFeatureV1;

/// Final decision fusion.  Deterministic denials always win; model output can
/// raise an allow to approval or denial only when the feature evidence agrees.
pub struct DecisionFusion;

impl DecisionFusion {
    pub fn fuse(
        base: &GuardDecision,
        fast: &FastGuardResult,
        prediction: &RiskPrediction,
        features: &AgentChainFeatureV1,
    ) -> GuardDecision {
        let mut fused = base.clone();
        fused.classifier_prediction = Some(prediction.clone());
        if !prediction.available {
            return fused;
        }

        if matches!(base.decision, Decision::Deny { .. }) || fast.immediate_deny {
            return fused;
        }

        let model_supports_deny = matches!(prediction.class, RiskClass::Critical)
            || (matches!(prediction.class, RiskClass::High)
                && (features.sensitive_to_external_flow
                    || features.external_sink_count > 0
                    || features.network_egress_count > 0));
        if model_supports_deny {
            fused.decision = Decision::deny("local classifier detected high-risk behavior flow");
            fused.risk_score = fused.risk_score.max(prediction.score);
            fused.reasons.push("local_classifier_high_risk".to_string());
            fused.evidence.push(format!(
                "classifier={} class={:?} score={:.2} confidence={:.2}",
                prediction.model_version, prediction.class, prediction.score, prediction.confidence
            ));
            fused.stage = GuardStage::DecisionFusion;
            return fused;
        }

        let model_requires_approval =
            matches!(prediction.class, RiskClass::High | RiskClass::Medium)
                && prediction.score >= 0.55;
        if model_requires_approval && matches!(base.decision, Decision::Allow) {
            fused.decision =
                Decision::require_approval("local classifier requires human confirmation");
            fused.risk_score = fused.risk_score.max(prediction.score);
            fused
                .reasons
                .push("local_classifier_requires_approval".to_string());
            fused.evidence.push(format!(
                "classifier={} class={:?} score={:.2} confidence={:.2}",
                prediction.model_version, prediction.class, prediction.score, prediction.confidence
            ));
            fused.stage = GuardStage::DecisionFusion;
        }
        fused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::GuardDecision;

    #[test]
    fn unavailable_model_is_observable_but_cannot_change_deterministic_decision() {
        let base = GuardDecision::allow_fast();
        let fast = FastGuardResult::allow();
        let prediction = RiskPrediction::unavailable();
        let features = AgentChainFeatureV1::default();
        let fused = DecisionFusion::fuse(&base, &fast, &prediction, &features);
        assert_eq!(fused.decision, base.decision);
        assert_eq!(fused.classifier_prediction, Some(prediction));
        assert_eq!(fused.stage, GuardStage::FastGuard);
    }

    #[test]
    fn high_risk_model_requires_supporting_sensitive_flow_before_denial() {
        let base = GuardDecision::allow_fast();
        let fast = FastGuardResult::allow();
        let prediction = RiskPrediction {
            class: RiskClass::High,
            score: 0.91,
            confidence: 0.8,
            model_version: "test-model".into(),
            available: true,
        };
        let mut features = AgentChainFeatureV1::default();
        features.sensitive_to_external_flow = true;
        let fused = DecisionFusion::fuse(&base, &fast, &prediction, &features);
        assert!(matches!(fused.decision, Decision::Deny { .. }));
        assert_eq!(fused.stage, GuardStage::DecisionFusion);
        assert!(fused.to_json()["classifier_prediction"]["available"]
            .as_bool()
            .unwrap());
    }
}
