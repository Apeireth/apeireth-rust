//! Canonical memory subsystem (M1B1 + M1B2 + M1B3).
//!
//! This module is the canonical owner of durable memory and its query
//! infrastructure on `reconstruct_v2`. It deliberately does not know about
//! runtime, gateway, provider, companion, or plugin registries.

pub mod domain;
pub mod error;
pub mod graph;
pub mod repository;
pub mod retrieval;
pub mod sqlite;
pub mod vector;

pub use crate::retrieval_pipeline::{
    BasicLexicalCandidateSource, HybridRetrievalPipeline, LexicalCandidateSource,
    MemoryCandidateSource, RetrievalStatus, StaticVectorCandidateSource, VectorCandidateSource,
};
pub use crate::scope::{
    DeterministicReranker, EmbeddingError, EmbeddingProvider, InMemoryPersonaProfileStore,
    MemoryCandidate, MemoryProvenance, MemoryRankingConfig, MemoryReranker, MemoryScope,
    NoEmbeddingProvider, PersonaMemoryProfile, PersonaProfileDelta, PersonaProfileStore,
    ScoreComponents,
};
pub use domain::{MemoryId, MemoryItem};
pub use error::MemoryError;
pub use graph::{Edge, MemoryGraph, Node};
pub use repository::{MemoryFilter, MemoryRepository};
pub use retrieval::{
    act_r_activation, retrieve, MemoryHit, RetrievalOptions, DEFAULT_ACT_R_BETA,
    DEFAULT_ACT_R_DECAY, DEFAULT_IMPORTANCE_WEIGHT,
};
pub use sqlite::SqliteMemoryRepository;
pub use vector::{
    content_hash, cosine_similarity, VectorHit, VectorIndex, VectorMetadataStore, VectorRecord,
};
