//! Composed Qdrant/FalkorDB implementation of the knowledge port.

use crate::config::KnowledgeConfig;
use crate::knowledge::{
    ChunkFilter, EntityRecord, Fact, FactCaps, KnowledgeError, KnowledgeStore, Predicate,
    ScoredHit, VectorSpace,
};

use super::falkor::{EntityDeleteOutcome, FalkorGraph};
use super::qdrant::QdrantStore;

/// The production knowledge adapter: Qdrant owns vectors and `FalkorDB` owns the
/// entity graph. The worker creates it writable; API/query processes create it
/// read-only at the application boundary.
pub struct RemoteKnowledgeStore {
    vectors: QdrantStore,
    graph: FalkorGraph,
    read_only: bool,
}

impl RemoteKnowledgeStore {
    /// Connects to the configured Qdrant and `FalkorDB` services.
    ///
    /// # Errors
    /// Returns [`KnowledgeError`] when a service client cannot be created.
    pub fn connect(
        config: &KnowledgeConfig,
        dim: usize,
        read_only: bool,
    ) -> Result<Self, KnowledgeError> {
        let vectors = QdrantStore::new(
            config.qdrant_url().clone(),
            dim,
            config.timeout(),
            read_only,
        )?;
        let graph = FalkorGraph::new(
            config.falkordb_url(),
            config.graph_name(),
            config.timeout(),
            read_only,
        )?;
        Ok(Self {
            vectors,
            graph,
            read_only,
        })
    }

    /// Probes both external knowledge services.
    ///
    /// # Errors
    /// Returns [`KnowledgeError`] when either service is unavailable or returns
    /// an invalid health response.
    pub async fn health(&self) -> Result<(), KnowledgeError> {
        self.vectors.health().await?;
        self.graph.health().await
    }

    fn require_writable(&self) -> Result<(), KnowledgeError> {
        if self.read_only {
            Err(KnowledgeError::Unavailable(
                "knowledge store is read-only in this process".into(),
            ))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl KnowledgeStore for RemoteKnowledgeStore {
    fn capabilities(&self) -> crate::knowledge::KsCapabilities {
        crate::knowledge::KsCapabilities {
            filtered_ann: true,
            graph_traversal: true,
        }
    }

    async fn upsert_vectors(
        &self,
        space: VectorSpace,
        doc_id: &str,
        ids: &[&str],
        vectors: &[Vec<f32>],
    ) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.vectors.upsert(&space, doc_id, ids, vectors).await?;
        if matches!(space, VectorSpace::Chunks { .. }) {
            for id in ids {
                self.graph.ensure_chunk(id, doc_id).await?;
            }
        }
        Ok(())
    }

    async fn knn(
        &self,
        space: VectorSpace,
        query: &[f32],
        limit: usize,
        filter: &ChunkFilter,
    ) -> Result<Vec<ScoredHit>, KnowledgeError> {
        self.vectors.knn(&space, query, limit, filter).await
    }

    async fn has_vector(&self, space: VectorSpace, id: &str) -> Result<bool, KnowledgeError> {
        self.vectors.has_vector(&space, id).await
    }

    async fn upsert_entity(&self, entity: &EntityRecord) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.graph.upsert_entity(entity).await
    }

    async fn link_mention(&self, chunk_id: &str, entity_id: &str) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.graph.link_mention(chunk_id, entity_id).await
    }

    async fn fold_entity(&self, loser: &str, winner: &str) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.graph.fold_entity(loser, winner).await
    }

    async fn merge_fact(
        &self,
        subject_id: &str,
        predicate: Predicate,
        object_id: &str,
        evidence_chunk: &str,
        properties: Option<&serde_json::Value>,
        caps: FactCaps,
    ) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.graph
            .merge_fact(
                subject_id,
                predicate,
                object_id,
                evidence_chunk,
                properties,
                caps,
            )
            .await
    }

    async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.vectors.delete_document(doc_id).await?;
        self.graph.delete_document(doc_id).await
    }

    async fn delete_entity(&self, entity_id: &str) -> Result<bool, KnowledgeError> {
        self.require_writable()?;
        match self.graph.delete_entity(entity_id).await? {
            EntityDeleteOutcome::Deleted => {
                self.vectors.delete_entity(entity_id).await?;
                Ok(true)
            }
            EntityDeleteOutcome::Absent => {
                self.vectors.delete_entity(entity_id).await?;
                Ok(false)
            }
            EntityDeleteOutcome::Referenced => Ok(false),
        }
    }

    async fn chunks_for_entities(&self, ids: &[&str]) -> Result<Vec<String>, KnowledgeError> {
        self.graph.chunks_for_entities(ids).await
    }

    async fn facts_within_hops(&self, ids: &[&str], hops: u8) -> Result<Vec<Fact>, KnowledgeError> {
        self.graph.facts_within_hops(ids, hops).await
    }
}
