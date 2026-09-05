//! Governed hybrid retrieval pipeline.
//!
//! The pipeline is intentionally provider-neutral: lexical and vector
//! candidate sources expose safe candidate metadata, while Assembly decides
//! whether an embedding adapter or model reranker is available.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;

use crate::{
    MemoryCandidate, MemoryError, MemoryRankingConfig, MemoryReranker, MemoryScope, ScoreComponents,
};

/// Candidate source shared by lexical, vector, working, episodic, semantic,
/// and relational implementations.
pub trait MemoryCandidateSource: Send + Sync {
    fn candidates(&self, query: &str, limit: usize) -> Result<Vec<MemoryCandidate>, MemoryError>;
}

/// Marker for a lexical/BM25 source.
pub trait LexicalCandidateSource: MemoryCandidateSource {}

/// Marker for a semantic/vector source.
pub trait VectorCandidateSource: MemoryCandidateSource {}

/// A small deterministic lexical source suitable for local BM25 fallback and
/// unit tests. It uses Unicode-aware tokens rather than ASCII whitespace.
#[derive(Debug, Clone, Default)]
pub struct BasicLexicalCandidateSource {
    documents: Vec<MemoryCandidate>,
}

impl BasicLexicalCandidateSource {
    pub fn new(documents: Vec<MemoryCandidate>) -> Self {
        Self { documents }
    }

    pub fn push(&mut self, candidate: MemoryCandidate) {
        self.documents.push(candidate);
    }
}

impl MemoryCandidateSource for BasicLexicalCandidateSource {
    fn candidates(&self, query: &str, limit: usize) -> Result<Vec<MemoryCandidate>, MemoryError> {
        let query_tokens = unicode_tokens(query);
        let mut out = self.documents.clone();
        for candidate in &mut out {
            let tokens = unicode_tokens(&candidate.content);
            candidate.score_components.lexical = bm25_like_score(&query_tokens, &tokens);
            candidate.score = candidate.score_components.lexical;
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        out.truncate(limit);
        Ok(out)
    }
}

impl LexicalCandidateSource for BasicLexicalCandidateSource {}

/// An already-embedded candidate source. Embedding creation remains outside
/// the Memory crate; this type only consumes validated candidate metadata.
#[derive(Debug, Clone, Default)]
pub struct StaticVectorCandidateSource {
    documents: Vec<MemoryCandidate>,
}

impl StaticVectorCandidateSource {
    pub fn new(documents: Vec<MemoryCandidate>) -> Self {
        Self { documents }
    }
}

impl MemoryCandidateSource for StaticVectorCandidateSource {
    fn candidates(&self, _query: &str, limit: usize) -> Result<Vec<MemoryCandidate>, MemoryError> {
        let mut out = self.documents.clone();
        out.sort_by(|a, b| {
            b.score_components
                .semantic
                .partial_cmp(&a.score_components.semantic)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        out.truncate(limit);
        Ok(out)
    }
}

impl VectorCandidateSource for StaticVectorCandidateSource {}

/// Output of the hybrid pipeline, including whether the vector stage was
/// available so status projections can be truthful.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetrievalStatus {
    pub lexical_candidates: usize,
    pub vector_candidates: usize,
    pub used_lexical_fallback: bool,
    pub reranked: bool,
}

/// Deterministic two-stage retrieval with explicit governance and budgets.
#[derive(Debug, Clone)]
pub struct HybridRetrievalPipeline {
    pub ranking: MemoryRankingConfig,
    pub candidate_cap: usize,
    pub max_rerank_tokens: usize,
}

impl Default for HybridRetrievalPipeline {
    fn default() -> Self {
        Self {
            ranking: MemoryRankingConfig::default(),
            candidate_cap: 64,
            max_rerank_tokens: 1_024,
        }
    }
}

impl HybridRetrievalPipeline {
    pub fn new(ranking: MemoryRankingConfig) -> Self {
        Self {
            ranking,
            ..Self::default()
        }
    }

    /// Run scope filter, union, deduplication, ranking, diversity, and budget
    /// without mutating storage or access metadata.
    pub fn retrieve(
        &self,
        query: &str,
        visible_scopes: &[MemoryScope],
        sources: &[&dyn MemoryCandidateSource],
        limit: usize,
        max_chars: usize,
    ) -> Result<Vec<MemoryCandidate>, MemoryError> {
        self.retrieve_with_status(query, visible_scopes, sources, limit, max_chars)
            .map(|(items, _)| items)
    }

    pub fn retrieve_with_status(
        &self,
        query: &str,
        visible_scopes: &[MemoryScope],
        sources: &[&dyn MemoryCandidateSource],
        limit: usize,
        max_chars: usize,
    ) -> Result<(Vec<MemoryCandidate>, RetrievalStatus), MemoryError> {
        let mut by_id: HashMap<String, MemoryCandidate> = HashMap::new();
        let mut status = RetrievalStatus::default();
        for source in sources {
            let candidates = source.candidates(query, self.candidate_cap)?;
            for mut candidate in candidates {
                if !candidate.scope.is_visible_in(visible_scopes) {
                    continue;
                }
                if candidate.score_components.semantic > 0.0 {
                    status.vector_candidates += 1;
                } else {
                    status.lexical_candidates += 1;
                }
                candidate.score = candidate.score_components.weighted(&self.ranking);
                match by_id.get_mut(&candidate.id) {
                    Some(existing) => {
                        existing.score_components.semantic = existing
                            .score_components
                            .semantic
                            .max(candidate.score_components.semantic);
                        existing.score_components.lexical = existing
                            .score_components
                            .lexical
                            .max(candidate.score_components.lexical);
                        existing.score = existing.score_components.weighted(&self.ranking);
                    }
                    None => {
                        by_id.insert(candidate.id.clone(), candidate);
                    }
                }
            }
        }
        status.used_lexical_fallback =
            status.vector_candidates == 0 && status.lexical_candidates > 0;
        let mut candidates: Vec<_> = by_id.into_values().collect();
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut seen_content = HashSet::new();
        let mut result = Vec::new();
        let mut chars = 0;
        for candidate in candidates {
            if result.len() >= limit {
                break;
            }
            let normalized: String = candidate
                .content
                .chars()
                .filter(|ch| ch.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            if normalized.is_empty() || !seen_content.insert(normalized) {
                continue;
            }
            if chars + candidate.content.len() > max_chars && !result.is_empty() {
                break;
            }
            chars += candidate.content.len();
            result.push(candidate);
        }
        Ok((result, status))
    }

    /// Optional model reranking is bounded and falls back to deterministic
    /// ranking if the adapter fails by returning an empty result.
    pub async fn retrieve_with_reranker(
        &self,
        query: &str,
        visible_scopes: &[MemoryScope],
        sources: &[&dyn MemoryCandidateSource],
        limit: usize,
        max_chars: usize,
        reranker: Option<&dyn MemoryReranker>,
    ) -> Result<(Vec<MemoryCandidate>, RetrievalStatus), MemoryError> {
        let (mut items, mut status) =
            self.retrieve_with_status(query, visible_scopes, sources, limit, max_chars)?;
        if let Some(reranker) = reranker {
            let reranked = reranker
                .rerank(query, items.clone(), self.max_rerank_tokens)
                .await;
            if !reranked.is_empty() || items.is_empty() {
                items = reranked;
                status.reranked = true;
            }
        }
        Ok((items, status))
    }
}

fn bm25_like_score(query: &[String], document: &[String]) -> f64 {
    if query.is_empty() || document.is_empty() {
        return 0.0;
    }
    let unique: HashSet<&String> = document.iter().collect();
    let matched = query.iter().filter(|token| unique.contains(token)).count();
    matched as f64 / query.len() as f64
}

/// Unicode fallback tokenizer: alphanumeric runs for Latin text and one
/// character tokens for CJK text.
pub fn unicode_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut run = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if is_cjk(ch) {
                if !run.is_empty() {
                    tokens.push(run.to_lowercase());
                    run.clear();
                }
                tokens.push(ch.to_string());
            } else {
                run.push(ch);
            }
        } else if !run.is_empty() {
            tokens.push(run.to_lowercase());
            run.clear();
        }
    }
    if !run.is_empty() {
        tokens.push(run.to_lowercase());
    }
    tokens
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryProvenance, ScoreComponents};

    fn candidate(id: &str, content: &str, scope: MemoryScope) -> MemoryCandidate {
        MemoryCandidate {
            id: id.into(),
            layer: "episodic".into(),
            scope,
            content: content.into(),
            score: 0.0,
            score_components: ScoreComponents::default(),
            provenance: MemoryProvenance::default(),
        }
    }

    #[test]
    fn chinese_lexical_fallback_and_scope_filter_are_deterministic() {
        let source = BasicLexicalCandidateSource::new(vec![
            candidate(
                "a",
                "项目 Alpha 的记忆",
                MemoryScope::Project {
                    project_id: "a".into(),
                },
            ),
            candidate(
                "b",
                "项目 Beta 的记忆",
                MemoryScope::Project {
                    project_id: "b".into(),
                },
            ),
        ]);
        let pipeline = HybridRetrievalPipeline::default();
        let items = pipeline
            .retrieve(
                "项目",
                &[MemoryScope::Project {
                    project_id: "a".into(),
                }],
                &[&source],
                4,
                400,
            )
            .unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
    }
}
