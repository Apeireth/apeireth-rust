//! Memory consolidation background job.
//!
//! Organizes, summarizes, and clusters fragmented episodic memories into cohesive semantic records.

use serde::{Deserialize, Serialize};

use apeireth_core::kernel::memory::Episode;

use crate::memory_governance::{MemoryGovernanceStatus, MemoryGovernanceStore};
use crate::MemoryError;

/// Report summarizing memory consolidation execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolidationReport {
    pub session_id: String,
    pub episodes_evaluated: usize,
    pub clusters_formed: usize,
    pub user_requests: usize,
    pub tool_invocations: usize,
    pub extracted_insights: Vec<String>,
}

/// Governed consolidation result. Forgotten source episodes are excluded
/// before deduplication, so derived output cannot resurrect them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryConsolidationOutput {
    pub merged: Vec<String>,
    pub promoted: Vec<String>,
    pub discarded: Vec<String>,
    pub profile_updates: Vec<String>,
    pub relation_updates: Vec<String>,
}

/// Offline or background consolidation job.
#[derive(Debug, Default, Clone)]
pub struct MemoryConsolidationJob;

impl MemoryConsolidationJob {
    pub fn new() -> Self {
        Self
    }

    /// Process a batch of episodes for a session and extract recurring topics / insights.
    pub fn consolidate(&self, session_id: &str, episodes: &[Episode]) -> ConsolidationReport {
        let mut extracted_insights = Vec::new();
        let mut tool_invocations = 0;
        let mut user_requests = 0;

        for ep in episodes {
            match ep.role.as_str() {
                "user" => user_requests += 1,
                "tool" => tool_invocations += 1,
                _ => {}
            }

            // Extract distinct error resolutions or accomplishments
            let content = &ep.content;
            if content.contains("error:")
                || content.contains("fixed")
                || content.contains("resolved")
            {
                let snippet: String = content
                    .lines()
                    .next()
                    .unwrap_or(content)
                    .chars()
                    .take(100)
                    .collect();
                if !extracted_insights.contains(&snippet) && extracted_insights.len() < 6 {
                    extracted_insights.push(snippet);
                }
            }
        }

        let clusters_formed = (episodes.len() / 5).max(1);

        ConsolidationReport {
            session_id: session_id.to_string(),
            episodes_evaluated: episodes.len(),
            clusters_formed,
            user_requests,
            tool_invocations,
            extracted_insights,
        }
    }

    /// Consolidate only governance-visible episodes. This is the production
    /// entry point for a deferred job; it performs no model call.
    pub fn consolidate_governed(
        &self,
        session_id: &str,
        episodes: &[Episode],
        governance: &dyn MemoryGovernanceStore,
    ) -> Result<MemoryConsolidationOutput, MemoryError> {
        let mut output = MemoryConsolidationOutput::default();
        let mut seen = std::collections::HashSet::new();
        for episode in episodes {
            if let Some(state) = governance
                .get_governed(&episode.id)
                .map_err(|error| MemoryError::Invalid(error.to_string()))?
            {
                if state.status == MemoryGovernanceStatus::Forgotten {
                    output.discarded.push(episode.id.clone());
                    continue;
                }
            }
            let normalized = episode.content.trim().to_lowercase();
            if !seen.insert(normalized) {
                output.merged.push(episode.id.clone());
                continue;
            }
            if episode.role == "user" {
                output.promoted.push(episode.id.clone());
            }
        }
        let _ = session_id;
        Ok(output)
    }
}
