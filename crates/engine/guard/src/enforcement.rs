//! Explicit enforcement directives derived from the canonical guard decision.
//!
//! This keeps policy interpretation separate from runtime side effects.  The
//! runtime already enforces `Decision`; these directives make the additional
//! containment obligations observable to Desktop/Gateway callers.

use serde::{Deserialize, Serialize};

use crate::decision::GuardDecision;
use apeireth_governance::Decision;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnforcementDirective {
    pub block_current_action: bool,
    pub require_approval: bool,
    pub revoke_session_grant: bool,
    pub mark_trace_high_risk: bool,
    pub compensation_hint: Option<String>,
}

impl EnforcementDirective {
    pub fn from_decision(decision: &GuardDecision) -> Self {
        let blocked = !decision.is_allowed();
        let denied = matches!(decision.decision, Decision::Deny { .. });
        let high_risk = decision.risk_score >= 0.85 || denied;
        Self {
            block_current_action: blocked,
            require_approval: matches!(decision.decision, Decision::RequireApproval { .. }),
            revoke_session_grant: denied && decision.risk_score >= 0.90,
            mark_trace_high_risk: high_risk,
            compensation_hint: denied.then(|| "review_prior_side_effects".to_string()),
        }
    }
}
