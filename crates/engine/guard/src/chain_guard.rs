//! Stage B — Chain Guard.
//!
//! Evaluates multi-step, compound, cross-resource, and escalation risks
//! across the entire behavior chain.

use apeireth_governance::Decision;

use crate::chain::BehaviorChain;
use crate::decision::{GuardDecision, GuardStage};
use crate::fast_guard::FastGuardResult;
use crate::observation::SafetyObservation;

/// Stage B Chain Guard.
#[derive(Debug, Default, Clone)]
pub struct ChainGuard;

impl ChainGuard {
    pub fn new() -> Self {
        Self
    }

    /// Evaluate the current action observation in the context of the accumulated behavior chain.
    pub fn evaluate(
        &self,
        chain: &BehaviorChain,
        obs: &SafetyObservation,
        fast_result: &FastGuardResult,
    ) -> GuardDecision {
        let mut reasons = fast_result.reasons.clone();
        let mut evidence = Vec::new();
        let mut risk_score = fast_result.risk_score;

        // If Fast Guard already issued an immediate deny, honour it with high certainty
        if fast_result.immediate_deny {
            return GuardDecision {
                decision: Decision::deny(
                    reasons
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "fast guard immediate denial".to_string()),
                ),
                risk_score: 1.0,
                reasons,
                evidence: vec!["fast_guard_immediate_violation".to_string()],
                stage: GuardStage::FastGuard,
                classifier_prediction: None,
            };
        }

        // 1. Privilege escalation check
        if chain.has_privilege_escalation() {
            reasons.push("privilege_escalation_detected".to_string());
            evidence.push(format!(
                "chain indicates progression from config discovery to credential retrieval to outbound execution: {} actions in chain",
                chain.actions().len()
            ));
            risk_score = risk_score.max(0.90);
        }

        // 2. Sensitive source -> external sink check
        if chain.has_sensitive_source_to_external_sink() {
            reasons.push("sensitive_source_to_external_sink".to_string());
            evidence.push(format!(
                "chain attempted sensitive data flow from private source to external sink for capability '{}'",
                obs.capability_id
            ));
            risk_score = risk_score.max(0.88);
        }

        // 3. Retry escalation check
        if chain.has_retry_escalation() {
            reasons.push("retry_escalation_after_denial".to_string());
            evidence.push(format!(
                "action '{}' follows previous denial with similar or elevated capability profile",
                obs.capability_id
            ));
            risk_score = risk_score.max(0.85);
        }

        // 4. Destructive chain check
        if chain.has_destructive_chain() {
            reasons.push("destructive_pipeline_chain".to_string());
            evidence.push(
                "chain combines deletion actions with external network or publishing tools"
                    .to_string(),
            );
            risk_score = risk_score.max(0.92);
        }

        // 5. Scope creep check against declared task scope
        if let Some(scope) = &chain.declared_task_scope {
            let scope_lc = scope.to_lowercase();
            if (scope_lc.contains("repo") || scope_lc.contains("code"))
                && (obs.capability_id.contains("system") || obs.capability_id.contains("network"))
            {
                reasons.push("scope_creep_outside_task_boundaries".to_string());
                evidence.push(format!(
                    "declared task scope was '{scope}', but action attempts external capability '{}'",
                    obs.capability_id
                ));
                risk_score = risk_score.max(0.70);
            }
        }

        // Map computed risk score to standard governance Decision:
        // - >= 0.85 -> Deny
        // - >= 0.40 -> RequireApproval
        // - < 0.40 -> Allow
        let decision = if risk_score >= 0.85 {
            let primary_reason = reasons
                .last()
                .cloned()
                .unwrap_or_else(|| "high-risk behavior chain violation".to_string());
            Decision::deny(format!("guard chain denied: {primary_reason}"))
        } else if risk_score >= 0.40 {
            let primary_reason = reasons
                .first()
                .cloned()
                .unwrap_or_else(|| "requires human verification".to_string());
            Decision::require_approval(format!(
                "guard chain requires human confirmation: {primary_reason}"
            ))
        } else {
            Decision::Allow
        };

        GuardDecision {
            decision,
            risk_score,
            reasons,
            evidence,
            stage: GuardStage::ChainGuard,
            classifier_prediction: None,
        }
    }
}
