//! Stage A — Fast Guard.
//!
//! Provides deterministic, sub-millisecond action-level risk detection
//! with zero LLM invocation. Evaluates dangerous operations, sensitive
//! sources and sinks, and declared task scope mismatches.

use crate::observation::{ResourceClass, SafetyObservation, SinkClass, SourceClass};

/// Outcome of the Fast Guard evaluation.
#[derive(Debug, Clone)]
pub struct FastGuardResult {
    /// Whether the action is completely clear of risk and may proceed immediately.
    pub clear: bool,
    /// Reasons identified if the action was flagged or requires escalation.
    pub reasons: Vec<String>,
    /// Estimated baseline risk score [0.0, 1.0].
    pub risk_score: f64,
    /// Whether the action is so overtly destructive that it should be denied immediately.
    pub immediate_deny: bool,
}

impl FastGuardResult {
    pub fn allow() -> Self {
        Self {
            clear: true,
            reasons: Vec::new(),
            risk_score: 0.0,
            immediate_deny: false,
        }
    }

    pub fn escalate(reasons: Vec<String>, risk_score: f64) -> Self {
        Self {
            clear: false,
            reasons,
            risk_score,
            immediate_deny: false,
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            clear: false,
            reasons: vec![reason.into()],
            risk_score: 1.0,
            immediate_deny: true,
        }
    }
}

/// Stage A deterministic Fast Guard.
#[derive(Debug, Default, Clone)]
pub struct FastGuard;

impl FastGuard {
    pub fn new() -> Self {
        Self
    }

    /// Evaluate an individual safety observation against deterministic safety rules.
    pub fn evaluate(
        &self,
        obs: &SafetyObservation,
        declared_scope: Option<&str>,
    ) -> FastGuardResult {
        let mut reasons = Vec::new();
        let mut max_risk: f64 = 0.0;

        // 1. Check for overtly destructive or forbidden shell/system commands
        if let Some(cmd_head) = obs
            .redacted_argument_features
            .get("command_head")
            .and_then(|v| v.as_str())
        {
            let head_lc = cmd_head.to_lowercase();
            if is_destructive_shell_command(&head_lc) {
                return FastGuardResult::deny(format!(
                    "dangerous destructive shell command: {head_lc}"
                ));
            }
        }

        // Check full command string if command features flagged dangerous flags
        if let Some(true) = obs
            .redacted_argument_features
            .get("has_command")
            .and_then(|v| v.as_bool())
        {
            let cap_str = obs.capability_id.to_lowercase();
            if cap_str.contains("shell") {
                reasons.push("shell_process_execution".to_string());
                max_risk = max_risk.max(0.65);
            }
        }

        // 2. Sensitive source access
        for src in &obs.source_classes {
            match src {
                SourceClass::CredentialStore => {
                    reasons.push("credential_store_access".to_string());
                    max_risk = max_risk.max(0.85);
                }
                SourceClass::Environment => {
                    reasons.push("environment_variables_access".to_string());
                    max_risk = max_risk.max(0.50);
                }
                SourceClass::PrivateFile => {
                    reasons.push("private_file_access".to_string());
                    max_risk = max_risk.max(0.70);
                }
                SourceClass::PrivateMemory => {
                    reasons.push("private_memory_access".to_string());
                    max_risk = max_risk.max(0.40);
                }
                _ => {}
            }
        }

        // 3. Sensitive sink egress
        for sink in &obs.sink_classes {
            match sink {
                SinkClass::ExternalNetwork => {
                    reasons.push("external_network_egress".to_string());
                    max_risk = max_risk.max(0.75);
                }
                SinkClass::ShellExecution => {
                    reasons.push("shell_sink_execution".to_string());
                    max_risk = max_risk.max(0.60);
                }
                _ => {}
            }
        }

        // 4. Scope mismatch
        if let Some(scope) = declared_scope {
            let scope_lc = scope.to_lowercase();
            if scope_lc.contains("read_only") || scope_lc.contains("readonly") {
                let cap_lc = obs.capability_id.to_lowercase();
                if obs.external_effect
                    || cap_lc.contains("write")
                    || cap_lc.contains("delete")
                    || cap_lc.contains("edit")
                    || cap_lc.contains("shell")
                {
                    return FastGuardResult::deny(format!(
                        "scope mismatch: task requested read-only, but agent requested mutation capability '{}'",
                        obs.capability_id
                    ));
                }
            }
        }

        // 5. Parent directory escape
        if let Some(true) = obs
            .redacted_argument_features
            .get("has_parent_traversal")
            .and_then(|v| v.as_bool())
        {
            reasons.push("parent_directory_traversal".to_string());
            max_risk = max_risk.max(0.80);
        }

        // 6. Dotfile / secret path access
        if let Some(true) = obs
            .redacted_argument_features
            .get("is_dotfile")
            .and_then(|v| v.as_bool())
        {
            reasons.push("dotfile_or_hidden_resource_access".to_string());
            max_risk = max_risk.max(0.55);
        }

        if reasons.is_empty() {
            FastGuardResult::allow()
        } else {
            FastGuardResult::escalate(reasons, max_risk)
        }
    }
}

fn is_destructive_shell_command(cmd: &str) -> bool {
    let clean = cmd.trim_end_matches(".exe");
    const FORBIDDEN: [&str; 9] = [
        "mkfs", "fdisk", "format", "dd", "reboot", "shutdown", "poweroff", "init", "halt",
    ];
    if clean.starts_with("mkfs") {
        return true;
    }
    FORBIDDEN.contains(&clean)
}
