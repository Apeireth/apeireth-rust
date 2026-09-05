//! Context-window projection and bounded compaction.
//!
//! Compaction only changes the next provider payload. It does not delete or
//! rewrite the persistent transcript and it is not a long-term memory write.

use apeireth_protocol::canonical::{ContentPart, MessageRole, NormalizedMessage};
use serde::{Deserialize, Serialize};

use crate::ContinuityState;

/// Config-driven context pressure policy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContextWindowPolicy {
    pub compact_threshold: f32,
    pub keep_recent_messages: usize,
    pub max_summary_chars: usize,
    pub reserved_output_tokens: usize,
}

impl Default for ContextWindowPolicy {
    fn default() -> Self {
        Self {
            compact_threshold: 0.80,
            keep_recent_messages: 4,
            max_summary_chars: 2_000,
            reserved_output_tokens: 512,
        }
    }
}

impl ContextWindowPolicy {
    fn validate(self) -> Result<Self, String> {
        if !(0.0..=1.0).contains(&self.compact_threshold) || self.compact_threshold == 0.0 {
            return Err("compact_threshold must be in (0, 1]".to_string());
        }
        if self.keep_recent_messages == 0 || self.max_summary_chars == 0 {
            return Err("context compaction limits must be greater than zero".to_string());
        }
        Ok(self)
    }
}

/// Provider-facing bounded projection of a persistent transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextWindow {
    pub messages: Vec<NormalizedMessage>,
    pub continuity: ContinuityState,
    pub compacted: bool,
    pub estimated_tokens: usize,
}

/// Stateless manager for making the next provider payload.
#[derive(Debug, Clone)]
pub struct ContextWindowManager {
    policy: ContextWindowPolicy,
}

impl Default for ContextWindowManager {
    fn default() -> Self {
        Self::new(ContextWindowPolicy::default()).expect("default context policy is valid")
    }
}

impl ContextWindowManager {
    pub fn new(policy: ContextWindowPolicy) -> Result<Self, String> {
        Ok(Self {
            policy: policy.validate()?,
        })
    }

    pub fn policy(&self) -> ContextWindowPolicy {
        self.policy
    }

    /// Returns true when the estimated input usage crosses the configured
    /// threshold after reserving output capacity.
    pub fn should_compact(&self, input_tokens: usize, model_context_tokens: usize) -> bool {
        if model_context_tokens == 0 {
            return false;
        }
        let usable = model_context_tokens.saturating_sub(self.policy.reserved_output_tokens);
        (input_tokens as f32) >= usable as f32 * self.policy.compact_threshold
    }

    /// Build a bounded provider projection. The input slice is never mutated.
    pub fn project(
        &self,
        transcript: &[NormalizedMessage],
        model_context_tokens: usize,
    ) -> ContextWindow {
        let estimated_tokens = estimate_tokens(transcript);
        if !self.should_compact(estimated_tokens, model_context_tokens)
            || transcript.len() <= self.policy.keep_recent_messages
        {
            return ContextWindow {
                messages: transcript.to_vec(),
                continuity: continuity_from_messages(transcript, self.policy.max_summary_chars),
                compacted: false,
                estimated_tokens,
            };
        }

        let split = transcript.len() - self.policy.keep_recent_messages;
        let older = &transcript[..split];
        let recent = &transcript[split..];
        let mut summary = String::new();
        for message in older {
            let text = ContentPart::join_text(&message.content);
            if text.trim().is_empty() {
                continue;
            }
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let line = format!("{role}: {}\n", first_line(&text));
            if summary.len() + line.len() > self.policy.max_summary_chars {
                break;
            }
            summary.push_str(&line);
        }

        let continuity = ContinuityState {
            session_id: "context-window".to_string(),
            summary: summary.clone(),
            key_entities: Vec::new(),
            active_constraints: Vec::new(),
            turn_count: transcript.len(),
            estimated_tokens: summary.len() / 4,
            rolling_summary: summary.clone(),
            active_goals: Vec::new(),
            unresolved_threads: Vec::new(),
            identity_anchors: Vec::new(),
            preference_deltas: Vec::new(),
            recent_entities: Vec::new(),
            revision: 1,
            updated_at: None,
        };
        let mut messages = Vec::with_capacity(recent.len() + 1);
        if !summary.is_empty() {
            messages.push(NormalizedMessage::system(format!(
                "<continuity_summary>{}</continuity_summary>",
                summary.trim_end()
            )));
        }
        messages.extend_from_slice(recent);
        ContextWindow {
            estimated_tokens: estimate_tokens(&messages),
            messages,
            continuity,
            compacted: true,
        }
    }
}

fn continuity_from_messages(
    messages: &[NormalizedMessage],
    max_summary_chars: usize,
) -> ContinuityState {
    let mut summary = String::new();
    for message in messages.iter().rev() {
        let text = ContentPart::join_text(&message.content);
        if text.trim().is_empty() {
            continue;
        }
        let line = first_line(&text);
        if summary.len() + line.len() + 1 > max_summary_chars {
            break;
        }
        if !summary.is_empty() {
            summary.insert(0, '\n');
        }
        summary.insert_str(0, &line);
    }
    ContinuityState {
        session_id: "context-window".to_string(),
        summary: summary.clone(),
        key_entities: Vec::new(),
        active_constraints: Vec::new(),
        turn_count: messages.len(),
        estimated_tokens: summary.len() / 4,
        rolling_summary: summary,
        active_goals: Vec::new(),
        unresolved_threads: Vec::new(),
        identity_anchors: Vec::new(),
        preference_deltas: Vec::new(),
        recent_entities: Vec::new(),
        revision: 0,
        updated_at: None,
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(240)
        .collect()
}

fn estimate_tokens(messages: &[NormalizedMessage]) -> usize {
    messages
        .iter()
        .map(|message| {
            ContentPart::join_text(&message.content)
                .chars()
                .count()
                .div_ceil(4)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_only_changes_provider_projection() {
        let manager = ContextWindowManager::default();
        let transcript = vec![
            NormalizedMessage::user("one"),
            NormalizedMessage::assistant("two"),
            NormalizedMessage::user("three"),
            NormalizedMessage::assistant("four"),
            NormalizedMessage::user("five"),
            NormalizedMessage::assistant("six"),
        ];
        let projection = manager.project(&transcript, 8);
        assert!(projection.compacted);
        assert_eq!(transcript.len(), 6);
        assert!(projection.messages.len() <= 5);
        assert!(
            projection
                .messages
                .iter()
                .any(|message| ContentPart::join_text(&message.content)
                    .contains("continuity_summary"))
        );
    }
}
