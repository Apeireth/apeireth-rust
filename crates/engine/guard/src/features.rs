//! Stable, desensitized Agent behavior-chain features.
//!
//! `AgentChainFeatureV1` is a versioned contract. Paths, prompts, file
//! contents, memory bodies, credentials, and reasoning text never cross this
//! boundary; only classes, counts, ratios, and hashed/normalized identities do.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::chain::{ActionStatus, BehaviorChain};
use crate::observation::{SinkClass, SourceClass};

pub const AGENT_CHAIN_FEATURE_V1: &str = "AgentChainFeatureV1";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentChainFeatureV1 {
    pub schema_version: String,
    pub action_count: u32,
    pub unique_capability_count: u32,
    pub read_count: u32,
    pub write_count: u32,
    pub delete_count: u32,
    pub execute_count: u32,
    pub network_count: u32,
    pub external_effect_count: u32,
    pub sensitive_source_count: u32,
    pub credential_access_count: u32,
    pub private_memory_read_count: u32,
    pub environment_access_count: u32,
    pub external_sink_count: u32,
    pub network_egress_count: u32,
    pub process_execution_count: u32,
    pub denied_count: u32,
    pub approval_count: u32,
    pub retry_count: u32,
    pub scope_mismatch_count: u32,
    pub path_escape_count: u32,
    pub max_chain_depth: u32,
    pub branch_count: u32,
    pub mutation_ratio: f64,
    pub read_write_ratio: f64,
    pub retry_after_denial: bool,
    pub alternate_tool_after_denial: bool,
    pub sensitive_to_external_flow: bool,
    pub source_sink_distance: u32,
    pub tool_transition_features: BTreeMap<String, u32>,
}

impl AgentChainFeatureV1 {
    pub fn from_chain(chain: &BehaviorChain) -> Self {
        let actions = chain.actions();
        let action_count = actions.len() as u32;
        let mut capabilities = std::collections::BTreeSet::new();
        let mut read_count = 0;
        let mut write_count = 0;
        let mut delete_count = 0;
        let mut execute_count = 0;
        let mut network_count = 0;
        let mut external_effect_count = 0;
        let mut sensitive_source_count = 0;
        let mut credential_access_count = 0;
        let mut private_memory_read_count = 0;
        let mut environment_access_count = 0;
        let mut external_sink_count = 0;
        let mut network_egress_count = 0;
        let mut process_execution_count = 0;
        let mut denied_count = 0;
        let mut approval_count = 0;
        let mut retry_count = 0;
        let mut scope_mismatch_count = 0;
        let mut path_escape_count = 0;
        let mut transitions = BTreeMap::new();

        for action in &actions {
            let cap = action.capability_id.to_lowercase();
            capabilities.insert(action.capability_id.clone());
            let is_read = cap.contains("read") || cap.contains("search") || cap.contains("list");
            let is_write = cap.contains("write") || cap.contains("edit") || cap.contains("update");
            let is_delete =
                cap.contains("delete") || cap.contains("remove") || cap.contains("unlink");
            let is_execute =
                cap.contains("shell") || cap.contains("exec") || cap.contains("process");
            let is_network =
                cap.contains("fetch") || cap.contains("http") || cap.contains("network");
            read_count += u32::from(is_read);
            write_count += u32::from(is_write);
            delete_count += u32::from(is_delete);
            execute_count += u32::from(is_execute);
            network_count += u32::from(is_network);
            external_effect_count += u32::from(action.external_effect);
            denied_count += u32::from(action.denied || action.status == ActionStatus::Denied);
            approval_count += u32::from(action.status == ActionStatus::RequireApproval);
            retry_count += u32::from(action.round > 1);
            credential_access_count +=
                u32::from(cap.contains("credential") || cap.contains("secret"));
            environment_access_count += u32::from(cap.contains("env"));
            process_execution_count += u32::from(is_execute);
            network_egress_count += u32::from(is_network && action.external_effect);
            scope_mismatch_count += u32::from(
                chain
                    .declared_task_scope
                    .as_deref()
                    .is_some_and(|scope| scope.to_lowercase().contains("read_only") && !is_read),
            );
            path_escape_count +=
                u32::from(cap.contains("path_escape") || cap.contains("traversal"));
            if let Some(previous) = actions
                .iter()
                .take_while(|candidate| candidate.id != action.id)
                .last()
            {
                let key = format!("{}>{}", previous.capability_id, action.capability_id);
                *transitions.entry(key).or_default() += 1;
            }
        }

        for node in &chain.nodes {
            if let crate::chain::BehaviorNode::Data(data) = node {
                let sensitive = matches!(
                    data.source,
                    SourceClass::PrivateFile
                        | SourceClass::PrivateMemory
                        | SourceClass::CredentialStore
                        | SourceClass::Environment
                );
                sensitive_source_count += u32::from(sensitive);
                private_memory_read_count += u32::from(data.source == SourceClass::PrivateMemory);
                external_sink_count += u32::from(matches!(
                    data.sink,
                    Some(SinkClass::ExternalNetwork | SinkClass::ShellExecution)
                ));
            }
        }
        let retry_after_denial = chain.has_retry_escalation();
        let alternate_tool_after_denial = actions.windows(2).any(|window| {
            window[0].denied
                && window[0].capability_id != window[1].capability_id
                && semantic_category(&window[0].capability_id)
                    == semantic_category(&window[1].capability_id)
        });
        let read_write_ratio = if read_count + write_count == 0 {
            0.0
        } else {
            f64::from(read_count) / f64::from(read_count + write_count)
        };
        let mutation_ratio = if action_count == 0 {
            0.0
        } else {
            f64::from(write_count + delete_count + execute_count) / f64::from(action_count)
        };
        Self {
            schema_version: AGENT_CHAIN_FEATURE_V1.to_string(),
            action_count,
            unique_capability_count: capabilities.len() as u32,
            read_count,
            write_count,
            delete_count,
            execute_count,
            network_count,
            external_effect_count,
            sensitive_source_count,
            credential_access_count,
            private_memory_read_count,
            environment_access_count,
            external_sink_count,
            network_egress_count,
            process_execution_count,
            denied_count,
            approval_count,
            retry_count,
            scope_mismatch_count,
            path_escape_count,
            max_chain_depth: action_count,
            branch_count: chain
                .edges
                .iter()
                .filter(|edge| edge.from != edge.to)
                .count()
                .saturating_sub(action_count.saturating_sub(1) as usize)
                as u32,
            mutation_ratio,
            read_write_ratio,
            retry_after_denial,
            alternate_tool_after_denial,
            sensitive_to_external_flow: chain.has_sensitive_source_to_external_sink(),
            source_sink_distance: if chain.has_sensitive_source_to_external_sink() {
                1
            } else {
                0
            },
            tool_transition_features: transitions,
        }
    }
}

fn semantic_category(capability: &str) -> &str {
    let cap = capability.to_lowercase();
    if cap.contains("network") || cap.contains("fetch") || cap.contains("http") {
        "network_egress"
    } else if cap.contains("shell") || cap.contains("exec") || cap.contains("process") {
        "process_execution"
    } else if cap.contains("file") || cap.contains("fs") {
        "filesystem"
    } else {
        "other"
    }
}

impl From<&BehaviorChain> for AgentChainFeatureV1 {
    fn from(value: &BehaviorChain) -> Self {
        Self::from_chain(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_schema_is_stable_and_does_not_include_raw_content() {
        let chain = BehaviorChain::new("session", "trace");
        let features = AgentChainFeatureV1::from_chain(&chain);
        assert_eq!(features.schema_version, AGENT_CHAIN_FEATURE_V1);
        let json = serde_json::to_string(&features).unwrap();
        assert!(!json.contains("prompt"));
    }
}
