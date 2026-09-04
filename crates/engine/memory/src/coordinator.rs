//! Unified Memory Coordinator (`MemoryCoordinator`).
//!
//! Serves as the central orchestrator across all 4 memory layers:
//! - Working Memory (in-memory fast ring-buffer)
//! - Episodic Memory (governed SQLite, active/forgotten status, content override)
//! - Semantic / Personal Memory (user preferences, profile cards)
//! - Relational / Temporal Memory (knowledge graph facts, entity links)

use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::sync::Arc;

use apeireth_core::kernel::memory::Episode;
use apeireth_core::kernel::SessionId;
use apeireth_plugin::experience::{AssociationStore, KnowledgeGraphStore};
use apeireth_plugin::memory_backend::MemoryBackend;
use apeireth_plugin::preference::PreferenceStore;
use std::sync::Mutex;

use crate::consolidation::{ConsolidationReport, MemoryConsolidationJob};
use crate::context_compiler::ClosedWorldContextCompiler;
use crate::continuity_state::{ContinuityCompressor, ContinuityState};
use crate::layers::{
    MemoryLayerKind, MemoryRecallQuery, MemoryRecallResult, MemoryWritebackEntry,
    RecalledMemoryItem,
};
use crate::memory_governance::{
    GovernedEpisode, MemoryGovernanceError, MemoryGovernanceStatus, MemoryGovernanceStore,
};
use crate::MemoryError;

const WORKING_RING_BUFFER_CAP: usize = 30;

/// Unified memory orchestrator coordinating all memory layers and pipelines.
pub struct MemoryCoordinator {
    backend: Arc<dyn MemoryBackend>,
    governance: Arc<dyn MemoryGovernanceStore>,
    working: Mutex<HashMap<String, VecDeque<Episode>>>,
    preferences: Option<Arc<dyn PreferenceStore>>,
    graph: Option<Arc<dyn KnowledgeGraphStore>>,
    associations: Option<Arc<dyn AssociationStore>>,
    compressor: ContinuityCompressor,
    compiler: ClosedWorldContextCompiler,
    consolidation: MemoryConsolidationJob,
}

impl MemoryCoordinator {
    /// Create a new memory coordinator with core backend and governance store.
    pub fn new(
        backend: Arc<dyn MemoryBackend>,
        governance: Arc<dyn MemoryGovernanceStore>,
    ) -> Self {
        Self {
            backend,
            governance,
            working: Mutex::new(HashMap::new()),
            preferences: None,
            graph: None,
            associations: None,
            compressor: ContinuityCompressor::new(),
            compiler: ClosedWorldContextCompiler::new(),
            consolidation: MemoryConsolidationJob::new(),
        }
    }

    /// Attach optional semantic preference store.
    #[must_use]
    pub fn with_preferences(mut self, preferences: Arc<dyn PreferenceStore>) -> Self {
        self.preferences = Some(preferences);
        self
    }

    /// Attach optional experience and relational stores.
    #[must_use]
    pub fn with_experience(
        mut self,
        graph: Arc<dyn KnowledgeGraphStore>,
        associations: Arc<dyn AssociationStore>,
    ) -> Self {
        self.graph = Some(graph);
        self.associations = Some(associations);
        self
    }

    /// Reference to the underlying governance store for direct mutations.
    pub fn governance(&self) -> &dyn MemoryGovernanceStore {
        self.governance.as_ref()
    }

    /// Reference to the underlying memory backend.
    pub fn backend(&self) -> &dyn MemoryBackend {
        self.backend.as_ref()
    }

    /// Execute the Unified Recall Pipeline across requested layers.
    pub fn recall(&self, query: &MemoryRecallQuery) -> Result<MemoryRecallResult, MemoryError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut candidates = Vec::new();
        let mut governance_filtered = 0;

        // 1. Candidate Generation
        // Layer 1: Working Memory
        if query.layers.contains(&MemoryLayerKind::Working) {
            let working_lock = self.working.lock().expect("working memory mutex");
            if let Some(session_episodes) = working_lock.get(&query.session_id) {
                for ep in session_episodes.iter().rev().take(query.limit) {
                    if let Ok(Some(gov)) = self.governance.get_governed(&ep.id) {
                        if gov.status == MemoryGovernanceStatus::Forgotten {
                            governance_filtered += 1;
                            continue;
                        }
                    }
                    candidates.push(RecalledMemoryItem {
                        id: ep.id.clone(),
                        layer: MemoryLayerKind::Working,
                        content: ep.content.clone(),
                        timestamp_ms: ep.timestamp,
                        score: 0.0,
                        importance: 0.8,
                        source_ref: Some(format!("working:{}", ep.session_id)),
                    });
                }
            }
        }

        // Layer 2: Episodic Memory (Governed SQLite)
        if query.layers.contains(&MemoryLayerKind::Episodic) {
            if let Ok(raw_eps) = self
                .backend
                .recent_episodes(&query.session_id, query.limit * 2)
            {
                for ep in raw_eps {
                    let mut content = ep.content.clone();
                    let mut importance = 0.5;

                    if let Ok(Some(gov)) = self.governance.get_governed(&ep.id) {
                        if gov.status == MemoryGovernanceStatus::Forgotten {
                            governance_filtered += 1;
                            continue;
                        }
                        if let Some(c_override) = gov.content_override {
                            content = c_override;
                        }
                        if gov.protected {
                            importance = 0.9;
                        }
                    }

                    candidates.push(RecalledMemoryItem {
                        id: ep.id.clone(),
                        layer: MemoryLayerKind::Episodic,
                        content,
                        timestamp_ms: ep.timestamp,
                        score: 0.0,
                        importance,
                        source_ref: Some(format!("episodic:{}", ep.session_id)),
                    });
                }
            }
        }

        // Layer 3: Semantic / Personal Memory (Preferences)
        if query.layers.contains(&MemoryLayerKind::Semantic) {
            if let Some(pref_store) = &self.preferences {
                let session_id_parsed =
                    SessionId::from_str(&query.session_id).unwrap_or_else(|_| SessionId::new());
                if let Ok(prefs) = pref_store.recall_for_context(
                    &session_id_parsed,
                    &query.query_text,
                    query.limit as u32,
                ) {
                    for pref in prefs {
                        candidates.push(RecalledMemoryItem {
                            id: format!("pref:{}", pref.id),
                            layer: MemoryLayerKind::Semantic,
                            content: format!("Topic: {}. Preference: {}", pref.topic, pref.stance),
                            timestamp_ms: pref.created_at,
                            score: 0.0,
                            importance: pref.confidence.clamp(0.1, 1.0),
                            source_ref: Some(format!("preference:{}", pref.id)),
                        });
                    }
                }
            }
        }

        // Layer 4: Relational / Temporal Memory (Graph & Associations)
        if query.layers.contains(&MemoryLayerKind::Relational)
            && !query.query_text.trim().is_empty()
        {
            if let Some(graph_store) = &self.graph {
                if let Ok(facts) = graph_store.facts_from(&query.query_text, query.limit as u32) {
                    for fact in facts {
                        candidates.push(RecalledMemoryItem {
                            id: format!(
                                "fact:{}:{}:{}",
                                fact.subject_id, fact.predicate, fact.object_id
                            ),
                            layer: MemoryLayerKind::Relational,
                            content: format!(
                                "{} {} {}",
                                fact.subject_id, fact.predicate, fact.object_id
                            ),
                            timestamp_ms: now_ms,
                            score: 0.0,
                            importance: 0.6,
                            source_ref: Some("knowledge_graph".to_string()),
                        });
                    }
                }
            }
        }

        let total_candidates = candidates.len();

        // 2. Multi-factor Ranking
        let query_tokens: Vec<String> = query
            .query_text
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|s| s.len() > 1)
            .collect();

        for item in &mut candidates {
            // Keyword match ratio
            let item_lower = item.content.to_lowercase();
            let matched_tokens = if query_tokens.is_empty() {
                1.0
            } else {
                let matches = query_tokens
                    .iter()
                    .filter(|token| item_lower.contains(token.as_str()))
                    .count();
                matches as f64 / query_tokens.len() as f64
            };
            let s_rel = matched_tokens.clamp(0.1, 1.0);

            // Recency decay: S_rec = exp(-lambda * delta_hours)
            let delta_hours = (now_ms - item.timestamp_ms).max(0) as f64 / (1000.0 * 3600.0);
            let s_rec = (-query.recency_decay_lambda * delta_hours)
                .exp()
                .clamp(0.1, 1.0);

            // Importance
            let s_imp = item.importance.clamp(0.1, 1.0);

            // Preference boost
            let s_pref = if item.layer == MemoryLayerKind::Semantic {
                1.0
            } else {
                0.0
            };

            // Combined weighted score
            item.score = 0.35 * s_rel + 0.35 * s_rec + 0.15 * s_imp + 0.15 * s_pref;
        }

        // 3. Diversity & Dedup
        let mut seen_contents = HashSet::new();
        let mut deduplicated = Vec::new();

        // Sort descending by score
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for item in candidates {
            let normalized_content: String = item
                .content
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();

            if normalized_content.is_empty() || seen_contents.contains(&normalized_content) {
                continue;
            }
            seen_contents.insert(normalized_content);
            deduplicated.push(item);
        }

        // 4. Budget Truncation
        let mut final_items = Vec::new();
        let mut total_chars = 0;

        for item in deduplicated {
            if item.score < query.min_score {
                continue;
            }
            if final_items.len() >= query.limit {
                break;
            }
            if total_chars + item.content.len() > query.max_chars && !final_items.is_empty() {
                break;
            }
            total_chars += item.content.len();
            final_items.push(item);
        }

        Ok(MemoryRecallResult {
            items: final_items,
            total_candidates,
            governance_filtered,
            total_chars,
        })
    }

    /// Persist turn writeback entry into Working and Episodic layers.
    pub fn writeback(&self, entry: &MemoryWritebackEntry) -> Result<String, MemoryError> {
        let episode_id = format!("ep-{}", uuid::Uuid::new_v4());
        let timestamp = entry
            .timestamp_ms
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

        let episode = Episode {
            id: episode_id.clone(),
            timestamp,
            role: entry.role.clone(),
            content: entry.content.clone(),
            session_id: entry.session_id.clone(),
        };

        // 1. Update working memory ring buffer
        {
            let mut working_lock = self.working.lock().expect("working memory mutex");
            let ring = working_lock
                .entry(entry.session_id.clone())
                .or_insert_with(|| VecDeque::with_capacity(WORKING_RING_BUFFER_CAP));
            if ring.len() >= WORKING_RING_BUFFER_CAP {
                ring.pop_front();
            }
            ring.push_back(episode.clone());
        }

        // 2. Persist to storage backend
        self.backend
            .put_episode(&episode)
            .map_err(|e| MemoryError::Invalid(e.to_string()))?;

        Ok(episode_id)
    }

    /// Compile a structured closed-world prompt overlay from a recall query.
    pub fn compile_prompt_overlay(
        &self,
        query: &MemoryRecallQuery,
    ) -> Result<Option<String>, MemoryError> {
        let recall_result = self.recall(query)?;
        Ok(self
            .compiler
            .compile(&recall_result, &query.session_id, query.max_chars))
    }

    /// Generate a bounded continuity state compression for a session.
    pub fn compress_continuity(
        &self,
        session_id: &str,
        max_summary_chars: usize,
    ) -> Result<ContinuityState, MemoryError> {
        let episodes = self
            .backend
            .recent_episodes(session_id, 50)
            .map_err(|e| MemoryError::Invalid(e.to_string()))?;
        Ok(self
            .compressor
            .compress(session_id, &episodes, max_summary_chars))
    }

    /// Run background or idle memory consolidation job for a session.
    pub fn run_consolidation(&self, session_id: &str) -> Result<ConsolidationReport, MemoryError> {
        let episodes = self
            .backend
            .recent_episodes(session_id, 100)
            .map_err(|e| MemoryError::Invalid(e.to_string()))?;
        Ok(self.consolidation.consolidate(session_id, &episodes))
    }

    /// Forget an episode via the governance sidecar.
    pub fn forget_episode(
        &self,
        episode_id: &str,
        reason: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.governance
            .forget_episode(episode_id, reason, expected_rev)
    }

    /// Protect an episode from automatic purging or forgetting.
    pub fn protect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.governance.protect_episode(episode_id, expected_rev)
    }

    /// Unprotect an episode.
    pub fn unprotect_episode(
        &self,
        episode_id: &str,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.governance.unprotect_episode(episode_id, expected_rev)
    }

    /// Update an episode's content override.
    pub fn update_episode_content(
        &self,
        episode_id: &str,
        new_content: &str,
        updated_by: Option<&str>,
        expected_rev: i64,
    ) -> Result<GovernedEpisode, MemoryGovernanceError> {
        self.governance
            .update_episode_content(episode_id, new_content, updated_by, expected_rev)
    }
}
