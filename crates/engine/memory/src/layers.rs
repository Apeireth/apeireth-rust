//! Logical 4-layer memory representations for Apeireth Unified Memory 2.0.

use serde::{Deserialize, Serialize};

use crate::{MemoryProvenance, MemoryScope};

/// The four canonical memory layers in Apeireth Unified Memory 2.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLayerKind {
    /// Working Memory: in-memory transient scratchpad, active turn items, fast ring-buffer.
    Working,
    /// Episodic Memory: governed append-only conversation timeline, subject to governance filtering.
    Episodic,
    /// Semantic / Personal Memory: user preferences, profile facts, long-term invariant knowledge.
    Semantic,
    /// Relational / Temporal Memory: entity knowledge graph, association links, cross-session continuity.
    Relational,
}

impl MemoryLayerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Relational => "relational",
        }
    }
}

/// A recalled memory candidate from one of the four memory layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecalledMemoryItem {
    /// Stable memory identifier (e.g. "ep-1234", "pref-5678", "fact-9012").
    pub id: String,
    /// Memory layer this item originated from.
    pub layer: MemoryLayerKind,
    /// Memory text content (governance override already applied if episodic).
    pub content: String,
    /// Timestamp in milliseconds (creation or update).
    pub timestamp_ms: i64,
    /// Normalized combined relevance and ranking score [0.0, 1.0].
    pub score: f64,
    /// Baseline importance weight [0.0, 1.0].
    pub importance: f64,
    /// Source provenance reference (session ID, file, topic).
    pub source_ref: Option<String>,
}

/// Query specification for unified memory recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecallQuery {
    pub session_id: String,
    pub query_text: String,
    pub layers: Vec<MemoryLayerKind>,
    pub limit: usize,
    pub max_chars: usize,
    pub recency_decay_lambda: f64,
    pub min_score: f64,
    /// Optional deterministic time probe. When absent, production uses the
    /// current clock for backward compatibility; tests should set it.
    #[serde(default)]
    pub as_of_ms: Option<i64>,
    /// Explicit closed-world visibility set. Legacy queries default to their
    /// own session plus Global and therefore cannot see another session.
    #[serde(default)]
    pub visible_scopes: Vec<MemoryScope>,
}

impl MemoryRecallQuery {
    pub fn new(session_id: impl Into<String>, query_text: impl Into<String>) -> Self {
        let session_id = session_id.into();
        Self {
            session_id: session_id.clone(),
            query_text: query_text.into(),
            layers: vec![
                MemoryLayerKind::Working,
                MemoryLayerKind::Episodic,
                MemoryLayerKind::Semantic,
                MemoryLayerKind::Relational,
            ],
            limit: 8,
            max_chars: 4000,
            recency_decay_lambda: 0.05,
            min_score: 0.10,
            as_of_ms: None,
            visible_scopes: vec![
                MemoryScope::Global,
                MemoryScope::Session {
                    session_id: session_id.clone(),
                },
            ],
        }
    }

    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    #[must_use]
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars.max(128);
        self
    }

    #[must_use]
    pub fn with_layers(mut self, layers: Vec<MemoryLayerKind>) -> Self {
        self.layers = layers;
        self
    }

    /// Replace the explicit visibility set used by the scope filter.
    #[must_use]
    pub fn with_visible_scopes(mut self, visible_scopes: Vec<MemoryScope>) -> Self {
        self.visible_scopes = visible_scopes;
        self
    }

    /// Add one visible scope while preserving deterministic insertion order.
    #[must_use]
    pub fn with_visible_scope(mut self, scope: MemoryScope) -> Self {
        if !self.visible_scopes.contains(&scope) {
            self.visible_scopes.push(scope);
        }
        self
    }

    /// Set the deterministic retrieval time probe.
    #[must_use]
    pub fn with_as_of_ms(mut self, as_of_ms: i64) -> Self {
        self.as_of_ms = Some(as_of_ms);
        self
    }
}

/// Unified memory recall execution outcome.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryRecallResult {
    pub items: Vec<RecalledMemoryItem>,
    pub total_candidates: usize,
    pub governance_filtered: usize,
    pub total_chars: usize,
}

/// Structured writeback entry to persist after turn completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryWritebackEntry {
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub importance: Option<f64>,
    pub timestamp_ms: Option<i64>,
    pub tags: Vec<String>,
    /// Scope assigned before persistence; defaults to the source session.
    #[serde(default)]
    pub scope: MemoryScope,
    /// Structured, non-secret provenance for the write.
    #[serde(default)]
    pub provenance: MemoryProvenance,
}

impl MemoryWritebackEntry {
    pub fn new(
        session_id: impl Into<String>,
        role: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let session_id = session_id.into();
        Self {
            session_id: session_id.clone(),
            role: role.into(),
            content: content.into(),
            importance: None,
            timestamp_ms: None,
            tags: Vec::new(),
            scope: MemoryScope::Session {
                session_id: session_id.clone(),
            },
            provenance: MemoryProvenance {
                source_session: Some(session_id),
                ..MemoryProvenance::default()
            },
        }
    }
}
