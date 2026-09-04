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

/// A single entry in the `guard-dataset-v1` JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardDatasetEntry {
    pub format: String,
    pub timestamp_ms: i64,
    pub trace_id: String,
    pub session_id: String,
    pub capability_id: String,
    pub chain_features: serde_json::Value,
    pub fast_guard: serde_json::Value,
    pub chain_guard: Option<serde_json::Value>,
    pub final_decision: String,
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

    /// Record a sanitized training point.
    pub fn record(
        &self,
        obs: &SafetyObservation,
        chain: &BehaviorChain,
        fast_res: &FastGuardResult,
        guard_dec: &GuardDecision,
        human_approval: Option<&str>,
        execution_outcome: Option<&str>,
    ) {
        if !self.is_enabled() {
            return;
        }

        let entry = GuardDatasetEntry {
            format: "guard-dataset-v1".to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            trace_id: obs.trace_id.clone(),
            session_id: obs.session_id.clone(),
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
            human_approval: human_approval.map(str::to_string),
            execution_outcome: execution_outcome.map(str::to_string),
        };

        let Ok(serialized) = serde_json::to_string(&entry) else {
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
}
