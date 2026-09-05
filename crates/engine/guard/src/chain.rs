//! In-memory behavior chain representation.
//!
//! Stores turn-local action, resource, and data nodes with directed edges
//! capturing temporal sequence, causal impact, data flows, and permission
//! dependencies.

use serde::{Deserialize, Serialize};

use crate::observation::{ResourceClass, SafetyObservation, SinkClass, SourceClass};

/// Directed edge types in a behavior chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Temporal,
    Causal,
    DataFlow,
    PermissionDependency,
}

/// Action node in the behavior chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionNode {
    pub id: String,
    pub round: u32,
    pub capability_id: String,
    pub tool_name: String,
    pub argument_shape: String,
    pub status: ActionStatus,
    pub denied: bool,
    pub external_effect: bool,
}

/// Status of an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Pending,
    Allowed,
    Denied,
    RequireApproval,
    Succeeded,
    Failed,
}

/// Resource node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceNode {
    pub id: String,
    pub class: ResourceClass,
    pub target: String,
}

/// Data node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataNode {
    pub id: String,
    pub source: SourceClass,
    pub sink: Option<SinkClass>,
    pub label: String,
}

/// Node variants in the behavior graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BehaviorNode {
    Action(ActionNode),
    Resource(ResourceNode),
    Data(DataNode),
}

/// Directed edge between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorEdge {
    pub from: String,
    pub to: String,
    pub edge_type: EdgeType,
    pub label: Option<String>,
}

/// Turn-local behavior chain for a single request / trace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BehaviorChain {
    pub trace_id: String,
    pub session_id: String,
    pub nodes: Vec<BehaviorNode>,
    pub edges: Vec<BehaviorEdge>,
    pub declared_task_scope: Option<String>,
}

impl BehaviorChain {
    /// Create a new behavior chain for a session and trace.
    pub fn new(session_id: impl Into<String>, trace_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            trace_id: trace_id.into(),
            nodes: Vec::new(),
            edges: Vec::new(),
            declared_task_scope: None,
        }
    }

    /// Set the declared user intent / task scope (e.g. "read_only", "explore", "edit").
    pub fn set_declared_scope(&mut self, scope: impl Into<String>) {
        self.declared_task_scope = Some(scope.into());
    }

    /// Add an action observation to the chain and wire temporal and dataflow edges.
    pub fn add_action(&mut self, obs: &SafetyObservation, round: u32) -> String {
        let sequence = self.actions().len() as u32;
        let action_id = format!("act:{}:{}:{}", obs.request_id, round, sequence);

        // Previous action for temporal edge
        let prev_action_id = self.nodes.iter().rev().find_map(|n| match n {
            BehaviorNode::Action(act) => Some(act.id.clone()),
            _ => None,
        });

        let action_node = ActionNode {
            id: action_id.clone(),
            round,
            capability_id: obs.capability_id.clone(),
            tool_name: obs.tool_name.clone(),
            argument_shape: obs.argument_shape.clone(),
            status: ActionStatus::Pending,
            denied: false,
            external_effect: obs.external_effect,
        };
        self.nodes.push(BehaviorNode::Action(action_node));

        if let Some(prev) = prev_action_id {
            self.edges.push(BehaviorEdge {
                from: prev,
                to: action_id.clone(),
                edge_type: EdgeType::Temporal,
                label: Some("seq".to_string()),
            });
        }

        // Add resource nodes & edges
        for res_class in &obs.resource_classes {
            let res_id = format!("res_{:?}_{}", res_class, self.nodes.len());
            self.nodes.push(BehaviorNode::Resource(ResourceNode {
                id: res_id.clone(),
                class: *res_class,
                target: obs.capability_id.clone(),
            }));
            self.edges.push(BehaviorEdge {
                from: action_id.clone(),
                to: res_id,
                edge_type: EdgeType::Causal,
                label: Some("accesses".to_string()),
            });
        }

        // Add data nodes & edges
        for src_class in &obs.source_classes {
            let data_id = format!("data_{:?}_{}", src_class, self.nodes.len());
            let sink_class = obs.sink_classes.first().copied();
            self.nodes.push(BehaviorNode::Data(DataNode {
                id: data_id.clone(),
                source: *src_class,
                sink: sink_class,
                label: format!("{:?}->{:?}", src_class, sink_class),
            }));
            self.edges.push(BehaviorEdge {
                from: data_id,
                to: action_id.clone(),
                edge_type: EdgeType::DataFlow,
                label: Some("consumes".to_string()),
            });
        }

        action_id
    }

    /// Update status of an action node.
    pub fn update_action_status(&mut self, action_id: &str, status: ActionStatus) {
        for node in &mut self.nodes {
            if let BehaviorNode::Action(act) = node {
                if act.id == action_id {
                    act.status = status;
                    if matches!(status, ActionStatus::Denied) {
                        act.denied = true;
                    }
                    break;
                }
            }
        }
    }

    /// Retrieve all action nodes.
    pub fn actions(&self) -> Vec<&ActionNode> {
        self.nodes
            .iter()
            .filter_map(|n| match n {
                BehaviorNode::Action(act) => Some(act),
                _ => None,
            })
            .collect()
    }

    /// Retrieve the most recent N actions.
    pub fn recent_actions(&self, n: usize) -> Vec<&ActionNode> {
        let all = self.actions();
        if all.len() <= n {
            all
        } else {
            all[all.len() - n..].to_vec()
        }
    }

    /// Check whether a sensitive source was read prior to an external sink write.
    pub fn has_sensitive_source_to_external_sink(&self) -> bool {
        let mut saw_sensitive_source = false;
        for node in &self.nodes {
            match node {
                BehaviorNode::Data(data) => {
                    if matches!(
                        data.source,
                        SourceClass::PrivateFile
                            | SourceClass::PrivateMemory
                            | SourceClass::CredentialStore
                            | SourceClass::Environment
                    ) {
                        saw_sensitive_source = true;
                    }
                    if saw_sensitive_source
                        && matches!(
                            data.sink,
                            Some(SinkClass::ExternalNetwork | SinkClass::ShellExecution)
                        )
                    {
                        return true;
                    }
                }
                BehaviorNode::Action(act) => {
                    let cap = act.capability_id.to_lowercase();
                    if cap.contains("credential") || cap.contains("secret") || cap.contains("env") {
                        saw_sensitive_source = true;
                    }
                    if saw_sensitive_source
                        && (cap.contains("fetch")
                            || cap.contains("http")
                            || cap.contains("curl")
                            || cap.contains("network"))
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Check for retry escalation pattern:
    /// An action was denied, followed immediately by parameter perturbation or alternative tool.
    pub fn has_retry_escalation(&self) -> bool {
        let actions = self.actions();
        if actions.len() < 2 {
            return false;
        }

        for window in actions.windows(2) {
            let prev = window[0];
            let curr = window[1];

            if prev.denied || prev.status == ActionStatus::Denied {
                // If the next action attempts shell, process, or external network right after a denial
                let curr_cap = curr.capability_id.to_lowercase();
                if curr_cap.contains("shell")
                    || curr_cap.contains("exec")
                    || curr_cap.contains("fetch")
                    || curr_cap.contains("fs.delete")
                    || curr_cap == prev.capability_id
                {
                    return true;
                }
            }
        }
        false
    }

    /// Check for privilege escalation chain:
    /// e.g. read config/env -> extract credential -> invoke network.
    pub fn has_privilege_escalation(&self) -> bool {
        let actions = self.actions();
        let mut read_config = false;
        let mut discovered_cred = false;

        for act in actions {
            let cap = act.capability_id.to_lowercase();
            if cap.contains("fs.read") || cap.contains("read_file") {
                read_config = true;
            }
            if read_config
                && (cap.contains("keyring") || cap.contains("secret") || cap.contains("credential"))
            {
                discovered_cred = true;
            }
            if discovered_cred
                && (cap.contains("fetch") || cap.contains("shell") || cap.contains("http"))
            {
                return true;
            }
        }
        false
    }

    /// Check for destructive sequence:
    /// e.g. discover -> modify -> delete -> publish.
    pub fn has_destructive_chain(&self) -> bool {
        let actions = self.actions();
        let mut saw_delete = false;
        let mut saw_external = false;

        for act in actions {
            let cap = act.capability_id.to_lowercase();
            if cap.contains("delete") || cap.contains("remove") || cap.contains("unlink") {
                saw_delete = true;
            }
            if saw_delete
                && (cap.contains("fetch") || cap.contains("network") || cap.contains("publish"))
            {
                saw_external = true;
            }
        }
        saw_delete && saw_external
    }

    /// Summarize chain features for ML readiness and data recording.
    pub fn extract_features(&self) -> serde_json::Value {
        let actions = self.actions();
        let denied_count = actions.iter().filter(|a| a.denied).count();
        let ext_count = actions.iter().filter(|a| a.external_effect).count();

        serde_json::json!({
            "total_nodes": self.nodes.len(),
            "total_edges": self.edges.len(),
            "action_count": actions.len(),
            "denied_count": denied_count,
            "external_effect_count": ext_count,
            "has_sensitive_egress": self.has_sensitive_source_to_external_sink(),
            "has_retry_escalation": self.has_retry_escalation(),
            "has_privilege_escalation": self.has_privilege_escalation(),
            "has_destructive_chain": self.has_destructive_chain(),
            "declared_scope": self.declared_task_scope,
        })
    }
}
