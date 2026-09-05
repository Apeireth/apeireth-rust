//! Deferred Memory Plane extraction contracts.
//!
//! Model-backed implementations are injected by Assembly. This module only
//! defines safe input/output shapes and a cheap deterministic fallback.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{MemoryError, MemoryProvenance, MemoryScope, PersonaProfileDelta};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractionClass {
    Preference,
    Fact,
    Event,
    Experience,
    Relation,
    PersonaDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExtractionMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExtractionInput {
    pub scope: MemoryScope,
    pub source_session: Option<String>,
    pub source_trace: Option<String>,
    pub source_request: Option<String>,
    pub messages: Vec<MemoryExtractionMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedMemory {
    pub class: ExtractionClass,
    pub content: String,
    pub confidence: f64,
    pub scope: MemoryScope,
    pub provenance: MemoryProvenance,
    pub source_trace: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryExtractionResult {
    pub preferences: Vec<ExtractedMemory>,
    pub facts: Vec<ExtractedMemory>,
    pub events: Vec<ExtractedMemory>,
    pub experiences: Vec<ExtractedMemory>,
    pub profile_delta: Option<PersonaProfileDelta>,
    pub relations: Vec<ExtractedMemory>,
}

#[async_trait]
pub trait MemoryExtractor: Send + Sync {
    async fn extract(
        &self,
        input: MemoryExtractionInput,
    ) -> Result<MemoryExtractionResult, MemoryError>;
}

/// Deterministic, no-side-call extractor used when deferred ML/model
/// extraction is not assembled.
#[derive(Debug, Default, Clone, Copy)]
pub struct RuleMemoryExtractor;

#[async_trait]
impl MemoryExtractor for RuleMemoryExtractor {
    async fn extract(
        &self,
        input: MemoryExtractionInput,
    ) -> Result<MemoryExtractionResult, MemoryError> {
        let provenance = MemoryProvenance {
            source: "rule_extractor".into(),
            source_session: input.source_session.clone(),
            source_trace: input.source_trace.clone(),
            source_request: input.source_request.clone(),
        };
        let mut result = MemoryExtractionResult::default();
        for message in input.messages {
            let content = message.content.trim();
            if content.is_empty() {
                continue;
            }
            let lower = content.to_lowercase();
            let class = if lower.contains("prefer")
                || lower.contains("喜欢")
                || lower.contains("不要")
                || lower.contains("never")
            {
                ExtractionClass::Preference
            } else if message.role == "user" {
                ExtractionClass::Fact
            } else {
                ExtractionClass::Event
            };
            let item = ExtractedMemory {
                class: class.clone(),
                content: content.chars().take(512).collect(),
                confidence: 0.5,
                scope: input.scope.clone(),
                provenance: provenance.clone(),
                source_trace: input.source_trace.clone(),
            };
            match class {
                ExtractionClass::Preference => result.preferences.push(item),
                ExtractionClass::Fact => result.facts.push(item),
                ExtractionClass::Event => result.events.push(item),
                ExtractionClass::Experience => result.experiences.push(item),
                ExtractionClass::Relation | ExtractionClass::PersonaDelta => {}
            }
        }
        Ok(result)
    }
}
