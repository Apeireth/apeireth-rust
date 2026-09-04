//! Bounded Continuity State and incremental compression.
//!
//! Preserves long-running session coherence without overflowing LLM context limits.

use serde::{Deserialize, Serialize};

use apeireth_core::kernel::memory::Episode;

/// Bounded continuity representation of a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityState {
    pub session_id: String,
    pub summary: String,
    pub key_entities: Vec<String>,
    pub active_constraints: Vec<String>,
    pub turn_count: usize,
    pub estimated_tokens: usize,
}

/// Deterministic compressor for bounding continuity state without expensive LLM side-calls.
#[derive(Debug, Default, Clone)]
pub struct ContinuityCompressor;

impl ContinuityCompressor {
    pub fn new() -> Self {
        Self
    }

    /// Compress a slice of episodes into a bounded continuity state.
    pub fn compress(
        &self,
        session_id: &str,
        episodes: &[Episode],
        max_summary_chars: usize,
    ) -> ContinuityState {
        let mut key_entities = Vec::new();
        let mut active_constraints = Vec::new();
        let mut summary_lines = Vec::new();

        for ep in episodes {
            let content = ep.content.trim();
            if content.is_empty() {
                continue;
            }

            // Detect constraints or preferences (e.g. "prefer", "must", "never", "do not")
            let lower = content.to_lowercase();
            if lower.contains("must")
                || lower.contains("prefer")
                || lower.contains("never")
                || lower.contains("do not")
            {
                let snippet: String = content.chars().take(120).collect();
                if !active_constraints.contains(&snippet) && active_constraints.len() < 8 {
                    active_constraints.push(snippet);
                }
            }

            // Detect mentions of code projects or files
            for word in content.split_whitespace() {
                if (word.contains('/')
                    || word.contains('\\')
                    || word.ends_with(".rs")
                    || word.ends_with(".ts")
                    || word.ends_with(".md"))
                    && !key_entities.contains(&word.to_string())
                    && key_entities.len() < 12
                {
                    key_entities.push(
                        word.trim_matches(|c: char| {
                            !c.is_alphanumeric() && c != '.' && c != '/' && c != '\\'
                        })
                        .to_string(),
                    );
                }
            }

            // Add short turn recap
            let first_line = content.lines().next().unwrap_or(content);
            let snippet: String = first_line.chars().take(80).collect();
            summary_lines.push(format!("{}: {}", ep.role, snippet));
        }

        let mut summary = String::new();
        for line in summary_lines.iter().rev() {
            if summary.len() + line.len() + 1 > max_summary_chars {
                break;
            }
            if !summary.is_empty() {
                summary.insert(0, '\n');
            }
            summary.insert_str(0, line);
        }

        let estimated_tokens = summary.len() / 4
            + active_constraints
                .iter()
                .map(|s| s.len() / 4)
                .sum::<usize>();

        ContinuityState {
            session_id: session_id.to_string(),
            summary,
            key_entities,
            active_constraints,
            turn_count: episodes.len(),
            estimated_tokens,
        }
    }
}
