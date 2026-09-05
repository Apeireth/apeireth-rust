//! Desensitized dataset recorder for offline ML classifier training.
//!
//! Produces JSONL records adhering to the `guard-dataset-v1` specification.
//! Strictly ensures that no raw secrets, credentials, private memory bodies,
//! or raw chain-of-thought are ever recorded.

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

/// A single entry in the `guard-dataset-v1` JSONL file (event-sourced).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum GuardDatasetRecord {
    Classification(ClassificationRecord),
    Outcome(OutcomeRecord),
}

/// Pre-dispatch safety evaluation snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationRecord {
    pub format: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub session_id: String,
    pub action_id: String,
    pub capability_id: String,
    pub chain_features: serde_json::Value,
    pub fast_guard: serde_json::Value,
    pub chain_guard: Option<serde_json::Value>,
    pub final_decision: String,
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
    pub trace_id: String,
    pub session_id: String,
    pub action_id: String,
    pub capability_id: String,
    pub features: serde_json::Value,
    pub fast_guard: serde_json::Value,
    pub chain_guard: Option<serde_json::Value>,
    pub final_guard_decision: String,
    pub human_approval: Option<String>,
    pub execution_outcome: Option<String>,
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
            final_decision: guard_dec.decision.label().to_string(),
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
        let mut outcomes_by_trace: std::collections::HashMap<String, Vec<OutcomeRecord>> =
            std::collections::HashMap::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<GuardDatasetRecord>(line) {
                match record {
                    GuardDatasetRecord::Classification(c) => classifications.push(c),
                    GuardDatasetRecord::Outcome(o) => {
                        outcomes_by_trace
                            .entry(o.trace_id.clone())
                            .or_default()
                            .push(o);
                    }
                }
            }
        }

        let mut samples = Vec::new();
        for c in classifications {
            let mut matched_approval = None;
            let mut matched_outcome = None;

            if let Some(trace_outcomes) = outcomes_by_trace.get(&c.trace_id) {
                for o in trace_outcomes {
                    if let Some(app) = &o.human_approval {
                        matched_approval = Some(app.clone());
                    }
                    if let Some(out) = &o.execution_outcome {
                        matched_outcome = Some(out.clone());
                    }
                }
            }

            samples.push(SupervisedTrainingSample {
                trace_id: c.trace_id,
                session_id: c.session_id,
                action_id: c.action_id,
                capability_id: c.capability_id,
                features: c.chain_features,
                fast_guard: c.fast_guard,
                chain_guard: c.chain_guard,
                final_guard_decision: c.final_decision,
                human_approval: matched_approval,
                execution_outcome: matched_outcome,
            });
        }

        samples
    }
}
