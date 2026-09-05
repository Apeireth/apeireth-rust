//! Canonical in-memory vector index (M1B3).
//!
//! This is a deliberately boring local vector index for memory/query
//! infrastructure. It accepts caller-provided `Vec<f32>` vectors only; it
//! knows nothing about embedding models, providers, API keys, or endpoints.
//!
//! The donor implementation (`origin/master:.../storage/src/vector.rs`) is
//! in-memory only, so this index makes no persistence promise.

use std::collections::HashMap;

use apeireth_core::kernel::Timestamp;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::domain::MemoryId;
use super::error::MemoryError;

/// Persistent metadata for one embedding. The content hash makes invalidation
/// explicit and the model/dimension pair prevents incompatible vectors from
/// sharing an index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorRecord {
    pub memory_id: MemoryId,
    pub model_id: String,
    pub dimension: usize,
    pub vector: Vec<f32>,
    pub content_hash: String,
    pub updated_at: Timestamp,
}

impl VectorRecord {
    pub fn new(
        memory_id: MemoryId,
        model_id: impl Into<String>,
        vector: Vec<f32>,
        content: &str,
        updated_at: Timestamp,
    ) -> Result<Self, MemoryError> {
        let dimension = vector.len();
        if dimension == 0 || vector.iter().any(|value| !value.is_finite()) {
            return Err(MemoryError::InvalidData(
                "embedding vector must be non-empty and finite".into(),
            ));
        }
        Ok(Self {
            memory_id,
            model_id: model_id.into(),
            dimension,
            vector,
            content_hash: content_hash(content),
            updated_at,
        })
    }

    pub fn validate_compatible(&self, model_id: &str, dimension: usize) -> Result<(), MemoryError> {
        if self.model_id != model_id
            || self.dimension != dimension
            || self.vector.len() != dimension
        {
            return Err(MemoryError::InvalidData(format!(
                "embedding metadata mismatch: stored model={} dimension={}, requested model={} dimension={}",
                self.model_id, self.dimension, model_id, dimension
            )));
        }
        if self.vector.iter().any(|value| !value.is_finite()) {
            return Err(MemoryError::InvalidData(
                "embedding vector contains a non-finite value".into(),
            ));
        }
        Ok(())
    }
}

/// SHA-256 content identity used to invalidate stale vectors.
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

/// Persistence contract for embedding metadata. The repository implementation
/// is additive; a missing row simply means lazy embedding is required.
#[async_trait]
pub trait VectorMetadataStore: Send + Sync {
    async fn get_vector(&self, memory_id: &MemoryId) -> Result<Option<VectorRecord>, MemoryError>;
    async fn upsert_vector(&self, record: VectorRecord) -> Result<(), MemoryError>;
    async fn remove_vector(&self, memory_id: &MemoryId) -> Result<(), MemoryError>;
}

/// A query hit: memory id plus cosine similarity score.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub id: MemoryId,
    pub score: f32,
}

/// Deterministic in-memory cosine-similarity vector index.
///
/// The dimension is fixed at construction and every inserted or query vector
/// must match it. Finite values are required; `NaN` and infinities are
/// rejected rather than allowed to poison the ordering.
#[derive(Debug, Clone)]
pub struct VectorIndex {
    dimension: usize,
    items: HashMap<MemoryId, Vec<f32>>,
}

impl VectorIndex {
    /// Creates an empty index for vectors of `dimension` dimensions.
    pub fn new(dimension: usize) -> Result<Self, MemoryError> {
        if dimension == 0 {
            return Err(MemoryError::InvalidData(
                "vector dimension must be greater than 0".into(),
            ));
        }
        Ok(Self {
            dimension,
            items: HashMap::new(),
        })
    }

    /// Returns the fixed vector dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Number of stored vectors.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` when the index contains no vectors.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Inserts a vector for `id`. Duplicate ids are rejected with
    /// [`MemoryError::Conflict`].
    pub fn insert(&mut self, id: MemoryId, vector: Vec<f32>) -> Result<(), MemoryError> {
        self.validate_vector(&vector)?;
        if self.items.contains_key(&id) {
            return Err(MemoryError::Conflict(format!(
                "vector already exists for memory {id}"
            )));
        }
        self.items.insert(id, vector);
        Ok(())
    }

    /// Replaces the vector for `id`. Missing ids fail with
    /// [`MemoryError::NotFound`].
    pub fn update(&mut self, id: &MemoryId, vector: Vec<f32>) -> Result<(), MemoryError> {
        self.validate_vector(&vector)?;
        if !self.items.contains_key(id) {
            return Err(MemoryError::NotFound(format!(
                "vector not found for memory {id}"
            )));
        }
        self.items.insert(id.clone(), vector);
        Ok(())
    }

    /// Removes the vector for `id`. Missing ids fail with
    /// [`MemoryError::NotFound`].
    pub fn remove(&mut self, id: &MemoryId) -> Result<(), MemoryError> {
        if self.items.remove(id).is_none() {
            return Err(MemoryError::NotFound(format!(
                "vector not found for memory {id}"
            )));
        }
        Ok(())
    }

    /// Returns the stored vector for `id`, if present.
    pub fn get(&self, id: &MemoryId) -> Option<&[f32]> {
        self.items.get(id).map(Vec::as_slice)
    }

    /// Queries the index with cosine similarity.
    ///
    /// Returns up to `top_k` hits ordered by score descending and then id
    /// ascending. `top_k = 0` returns an empty result.
    ///
    /// Zero vectors are handled explicitly: a zero query vector or a zero
    /// stored vector yields a similarity of `0.0` (the donor behaviour) rather
    /// than `NaN`.
    pub fn query(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>, MemoryError> {
        self.validate_vector(query)?;

        let mut hits: Vec<VectorHit> = self
            .items
            .iter()
            .map(|(id, vector)| VectorHit {
                id: id.clone(),
                score: cosine_similarity(query, vector),
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });

        if top_k == 0 {
            hits.clear();
        } else {
            hits.truncate(top_k);
        }

        Ok(hits)
    }

    fn validate_vector(&self, vector: &[f32]) -> Result<(), MemoryError> {
        if vector.len() != self.dimension {
            return Err(MemoryError::InvalidData(format!(
                "vector dimension mismatch: expected {}, got {}",
                self.dimension,
                vector.len()
            )));
        }
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(MemoryError::InvalidData(
                "vector values must be finite".into(),
            ));
        }
        Ok(())
    }
}

/// Cosine similarity between two equal-length vectors.
///
/// Returns `0.0` when either vector is zero. Callers are responsible for
/// length and finite-value validation; the index performs both before calling
/// this function.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> MemoryId {
        MemoryId::new(s).unwrap()
    }

    #[test]
    fn cosine_similarity_handles_known_values_and_zero_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]) - 0.0).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
    }

    #[test]
    fn query_returns_nearest_first_and_stable_tie_order() {
        let mut index = VectorIndex::new(2).unwrap();
        index.insert(id("a"), vec![1.0, 0.0]).unwrap();
        index.insert(id("b"), vec![0.0, 1.0]).unwrap();
        index.insert(id("c"), vec![1.0, 0.0]).unwrap();

        let hits = index.query(&[0.9, 0.1], 3).unwrap();
        assert_eq!(hits[0].id, id("a"));
        assert_eq!(hits[1].id, id("c"));
        assert_eq!(hits[2].id, id("b"));
        // a and c are identical vectors, so their equal score must be
        // tie-broken by id; both must beat the orthogonal vector b.
        assert!((hits[0].score - hits[1].score).abs() < 1e-6);
        assert!(hits[1].score > hits[2].score);
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let mut index = VectorIndex::new(2).unwrap();
        assert!(matches!(
            index.insert(id("a"), vec![1.0]),
            Err(MemoryError::InvalidData(_))
        ));
        assert!(matches!(
            index.query(&[1.0, 2.0, 3.0], 1),
            Err(MemoryError::InvalidData(_))
        ));
    }

    #[test]
    fn non_finite_values_are_rejected() {
        let mut index = VectorIndex::new(2).unwrap();
        assert!(matches!(
            index.insert(id("a"), vec![f32::NAN, 0.0]),
            Err(MemoryError::InvalidData(_))
        ));
        assert!(matches!(
            index.insert(id("a"), vec![f32::INFINITY, 0.0]),
            Err(MemoryError::InvalidData(_))
        ));
    }

    #[test]
    fn duplicate_id_insert_is_rejected_and_update_remove_are_defined() {
        let mut index = VectorIndex::new(2).unwrap();
        index.insert(id("a"), vec![1.0, 0.0]).unwrap();
        assert!(matches!(
            index.insert(id("a"), vec![0.0, 1.0]),
            Err(MemoryError::Conflict(_))
        ));

        index.update(&id("a"), vec![0.0, 1.0]).unwrap();
        assert_eq!(index.get(&id("a")).unwrap(), &[0.0, 1.0]);

        index.remove(&id("a")).unwrap();
        assert!(matches!(
            index.remove(&id("a")),
            Err(MemoryError::NotFound(_))
        ));
        assert!(matches!(
            index.update(&id("a"), vec![1.0, 0.0]),
            Err(MemoryError::NotFound(_))
        ));
    }

    #[test]
    fn top_k_bounds_are_deterministic() {
        let mut index = VectorIndex::new(1).unwrap();
        index.insert(id("a"), vec![1.0]).unwrap();
        index.insert(id("b"), vec![0.5]).unwrap();

        assert!(index.query(&[1.0], 0).unwrap().is_empty());
        assert_eq!(index.query(&[1.0], 1).unwrap().len(), 1);
        assert_eq!(index.query(&[1.0], 10).unwrap().len(), 2);
    }

    #[test]
    fn zero_dimension_is_rejected() {
        assert!(matches!(
            VectorIndex::new(0),
            Err(MemoryError::InvalidData(_))
        ));
    }
}
