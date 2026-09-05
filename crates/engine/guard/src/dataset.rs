//! Desensitized dataset recorder for offline ML classifier training.
//!
//! Produces JSONL records adhering to the `guard-dataset-v1` specification.
//! Strictly ensures that no raw secrets, credentials, private memory bodies,
//! or raw chain-of-thought are ever recorded.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::chain::BehaviorChain;
use crate::decision::GuardDecision;
use crate::fast_guard::FastGuardResult;
use crate::observation::SafetyObservation;

/// A single entry in the event-sourced Guard dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum GuardDatasetRecord {
    Classification(ClassificationRecord),
    Approval(ApprovalRecord),
    Execution(ExecutionRecord),
    Compensation(CompensationRecord),
    /// Legacy outcome rows remain readable so an additive upgrade never
    /// invalidates an existing dataset file.
    Outcome(OutcomeRecord),
}

/// Pre-dispatch safety evaluation snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationRecord {
    pub format: String,
    #[serde(default = "default_feature_schema_version")]
    pub feature_schema_version: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub session_id: String,
    pub action_id: String,
    pub capability_id: String,
    pub chain_features: serde_json::Value,
    pub fast_guard: serde_json::Value,
    pub chain_guard: Option<serde_json::Value>,
    #[serde(default)]
    pub classifier_prediction: Option<serde_json::Value>,
    pub final_decision: String,
    #[serde(default)]
    pub weak_label: bool,
}

fn default_feature_schema_version() -> String {
    "AgentChainFeatureV1".to_string()
}

/// Human approval lifecycle event correlated to one concrete action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub format: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub action_id: String,
    pub tool_call_id: String,
    pub approval_id: String,
    pub decision: String,
}

/// Capability execution lifecycle event correlated to one concrete action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub format: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub action_id: String,
    pub tool_call_id: String,
    pub outcome: String,
}

/// Compensation lifecycle event correlated to one concrete action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompensationRecord {
    pub format: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub action_id: String,
    pub outcome: String,
}

/// Post-execution runtime outcome snapshot (approved/rejected/success/failure).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub format: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub action_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub approval_id: Option<String>,
    pub human_approval: Option<String>,
    pub execution_outcome: Option<String>,
}

/// Complete supervised training sample correlated across classification and outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisedTrainingSample {
    pub feature_schema_version: String,
    pub trace_id: String,
    pub session_id: String,
    pub action_id: String,
    pub capability_id: String,
    pub features: serde_json::Value,
    pub fast_guard: serde_json::Value,
    pub chain_guard: Option<serde_json::Value>,
    pub classifier_prediction: Option<serde_json::Value>,
    pub final_guard_decision: String,
    pub human_approval: Option<String>,
    pub execution_outcome: Option<String>,
    pub compensation_outcome: Option<String>,
    pub weak_label: bool,
}

/// Thread-safe desensitized dataset recorder.
pub struct DatasetRecorder {
    enabled: AtomicBool,
    output_path: PathBuf,
    file_lock: Mutex<()>,
}

impl DatasetRecorder {
    /// Create a new dataset recorder. Default is disabled for privacy.
    pub fn new(output_path: impl AsRef<Path>) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            output_path: output_path.as_ref().to_path_buf(),
            file_lock: Mutex::new(()),
        }
    }

    /// Set enabled status.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Whether recording is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Record a pre-dispatch classification evaluation event.
    pub fn record_classification(
        &self,
        action_id: &str,
        obs: &SafetyObservation,
        chain: &BehaviorChain,
        fast_res: &FastGuardResult,
        guard_dec: &GuardDecision,
    ) {
        if !self.is_enabled() {
            return;
        }

        let record = GuardDatasetRecord::Classification(ClassificationRecord {
            format: "guard-dataset-v1".to_string(),
            feature_schema_version: default_feature_schema_version(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            trace_id: obs.trace_id.clone(),
            session_id: obs.session_id.clone(),
            action_id: action_id.to_string(),
            capability_id: obs.capability_id.clone(),
            chain_features: chain.extract_features(),
            fast_guard: serde_json::json!({
                "clear": fast_res.clear,
                "reasons": fast_res.reasons,
                "risk_score": fast_res.risk_score,
            }),
            chain_guard: Some(serde_json::json!({
                "decision": guard_dec.decision.label(),
                "risk_score": guard_dec.risk_score,
                "reasons": guard_dec.reasons,
                "evidence": guard_dec.evidence,
                "stage": guard_dec.stage,
            })),
            classifier_prediction: guard_dec
                .classifier_prediction
                .as_ref()
                .and_then(|prediction| serde_json::to_value(prediction).ok()),
            final_decision: guard_dec.decision.label().to_string(),
            weak_label: true,
        });

        self.write_line(&record);
    }

    /// Record an execution outcome event from runtime events or approvals.
    pub fn record_outcome(
        &self,
        trace_id: &str,
        action_id: Option<&str>,
        tool_call_id: Option<&str>,
        approval_id: Option<&str>,
        human_approval: Option<&str>,
        execution_outcome: Option<&str>,
    ) {
        if !self.is_enabled() {
            return;
        }

        let record = GuardDatasetRecord::Outcome(OutcomeRecord {
            format: "guard-dataset-v1".to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            trace_id: trace_id.to_string(),
            action_id: action_id.map(str::to_string),
            tool_call_id: tool_call_id.map(str::to_string),
            approval_id: approval_id.map(str::to_string),
            human_approval: human_approval.map(str::to_string),
            execution_outcome: execution_outcome.map(str::to_string),
        });

        self.write_line(&record);
    }

    /// Record an explicit approval event. The action identity is mandatory so
    /// several actions in one trace cannot inherit the wrong approval.
    pub fn record_approval(
        &self,
        trace_id: &str,
        action_id: &str,
        tool_call_id: &str,
        approval_id: &str,
        decision: &str,
    ) {
        if !self.is_enabled() {
            return;
        }
        self.write_line(&GuardDatasetRecord::Approval(ApprovalRecord {
            format: "guard-dataset-v2".to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            trace_id: trace_id.to_string(),
            action_id: action_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            approval_id: approval_id.to_string(),
            decision: decision.to_string(),
        }));
    }

    /// Record an explicit capability completion event.
    pub fn record_execution(
        &self,
        trace_id: &str,
        action_id: &str,
        tool_call_id: &str,
        outcome: &str,
    ) {
        if !self.is_enabled() {
            return;
        }
        self.write_line(&GuardDatasetRecord::Execution(ExecutionRecord {
            format: "guard-dataset-v2".to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            trace_id: trace_id.to_string(),
            action_id: action_id.to_string(),
            tool_call_id: tool_call_id.to_string(),
            outcome: outcome.to_string(),
        }));
    }

    /// Record a compensation result without storing the compensating payload.
    pub fn record_compensation(&self, trace_id: &str, action_id: &str, outcome: &str) {
        if !self.is_enabled() {
            return;
        }
        self.write_line(&GuardDatasetRecord::Compensation(CompensationRecord {
            format: "guard-dataset-v2".to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            trace_id: trace_id.to_string(),
            action_id: action_id.to_string(),
            outcome: outcome.to_string(),
        }));
    }

    /// Backward-compatible combined record function.
    pub fn record(
        &self,
        obs: &SafetyObservation,
        chain: &BehaviorChain,
        fast_res: &FastGuardResult,
        guard_dec: &GuardDecision,
        human_approval: Option<&str>,
        execution_outcome: Option<&str>,
    ) {
        let action_id = format!("act:{}:{}:0", obs.request_id, 0);
        self.record_classification(&action_id, obs, chain, fast_res, guard_dec);
        if human_approval.is_some() || execution_outcome.is_some() {
            self.record_outcome(
                &obs.trace_id,
                Some(&action_id),
                None,
                None,
                human_approval,
                execution_outcome,
            );
        }
    }

    fn write_line(&self, record: &GuardDatasetRecord) {
        let Ok(serialized) = serde_json::to_string(record) else {
            return;
        };

        let _guard = self.file_lock.lock();
        if let Some(parent) = self.output_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.output_path)
        {
            let _ = writeln!(file, "{serialized}");
        }
    }

    /// Read and correlate classifications and outcomes into complete supervised training samples.
    pub fn load_supervised_samples(&self) -> Vec<SupervisedTrainingSample> {
        let _guard = self.file_lock.lock();
        let Ok(content) = std::fs::read_to_string(&self.output_path) else {
            return Vec::new();
        };

        let mut classifications = Vec::new();
        let mut approvals: HashMap<(String, String), String> = HashMap::new();
        let mut executions: HashMap<(String, String), String> = HashMap::new();
        let mut compensations: HashMap<(String, String), String> = HashMap::new();
        let mut legacy_outcomes_by_trace: HashMap<String, Vec<OutcomeRecord>> = HashMap::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<GuardDatasetRecord>(line) {
                match record {
                    GuardDatasetRecord::Classification(c) => classifications.push(c),
                    GuardDatasetRecord::Approval(a) => {
                        approvals.insert((a.trace_id, a.action_id), a.decision);
                    }
                    GuardDatasetRecord::Execution(e) => {
                        executions.insert((e.trace_id, e.action_id), e.outcome);
                    }
                    GuardDatasetRecord::Compensation(c) => {
                        compensations.insert((c.trace_id, c.action_id), c.outcome);
                    }
                    GuardDatasetRecord::Outcome(o) => {
                        legacy_outcomes_by_trace
                            .entry(o.trace_id.clone())
                            .or_default()
                            .push(o);
                    }
                }
            }
        }

        let mut samples = Vec::new();
        for c in &classifications {
            let key = (c.trace_id.clone(), c.action_id.clone());
            let mut matched_approval = approvals.get(&key).cloned();
            let mut matched_outcome = executions.get(&key).cloned();
            let matched_compensation = compensations.get(&key).cloned();

            // Backward compatibility is intentionally narrow: a legacy
            // trace-only row may be used only when the trace has exactly one
            // classification. Multiple actions remain incomplete rather than
            // receiving an ambiguous label.
            let trace_classification_count = classifications
                .iter()
                .filter(|candidate| candidate.trace_id == c.trace_id)
                .count();
            if matched_approval.is_none() || matched_outcome.is_none() {
                if let Some(trace_outcomes) = legacy_outcomes_by_trace.get(&c.trace_id) {
                    if trace_classification_count == 1 {
                        for o in trace_outcomes {
                            if matched_approval.is_none() {
                                matched_approval = o.human_approval.clone();
                            }
                            if matched_outcome.is_none() {
                                matched_outcome = o.execution_outcome.clone();
                            }
                        }
                    }
                }
            }
            if trace_classification_count == 1 && matched_outcome.is_none() {
                matched_outcome = executions
                    .iter()
                    .find(|((trace_id, _), _)| trace_id == &c.trace_id)
                    .map(|(_, outcome)| outcome.clone());
            }

            samples.push(SupervisedTrainingSample {
                feature_schema_version: c.feature_schema_version.clone(),
                trace_id: c.trace_id.clone(),
                session_id: c.session_id.clone(),
                action_id: c.action_id.clone(),
                capability_id: c.capability_id.clone(),
                features: c.chain_features.clone(),
                fast_guard: c.fast_guard.clone(),
                chain_guard: c.chain_guard.clone(),
                classifier_prediction: c.classifier_prediction.clone(),
                final_guard_decision: c.final_decision.clone(),
                human_approval: matched_approval,
                execution_outcome: matched_outcome,
                compensation_outcome: matched_compensation,
                weak_label: c.weak_label,
            });
        }

        samples
    }
}
