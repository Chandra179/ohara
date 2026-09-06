//! KNOWLEDGE PLANE facade (§1.3) — owns all `LadybugDB` access (§1.2.2): vector
//! collections and the property graph, behind the [`KnowledgeStore`] port. `SQLite`
//! and `data/` are the system of record; everything behind this port is a
//! rebuildable index (§7.9).

#[cfg(feature = "ladybug")]
mod graph;
mod reconcile;
#[cfg(feature = "ladybug")]
mod vectors;

#[cfg(feature = "ladybug")]
pub use vectors::LadybugStore;

use async_trait::async_trait;

use crate::Class;

/// Embedding-model identity (§4, §11.1): the quantization variant is part of the
/// id, and one vector collection exists per id — mixing models in one collection
/// is structurally impossible.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelId(String);

impl ModelId {
    /// Validates at the boundary (§10): the id must be non-empty.
    ///
    /// # Panics
    /// On an empty id — a broken invariant, since config validates model ids at
    /// boot (§10's sanctioned `expect`-with-invariant class).
    pub fn new(raw: impl Into<String>) -> Self {
        let id = raw.into();
        assert!(
            !id.is_empty(),
            "invariant: model id is non-empty (validated at boot, §10)"
        );
        Self(id)
    }

    /// The id as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which vector collection a call addresses (§9): one per model (§4) plus entity
/// names. All vector ops are scoped by this — collection isolation is a
/// contract-tested property (§9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VectorSpace {
    /// Chunk vectors for one embedding model/variant.
    Chunks {
        /// The model whose collection is addressed.
        model_id: ModelId,
    },
    /// Entity-name vectors (entity-resolution matching, §8 Stage 4).
    EntityNames,
}

/// Entity supertypes — the closed ontology (§5 CHECK, §8 Stage 4). Date/Time is an
/// edge property (`occurred_on`), never a node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityType {
    /// A person.
    Person,
    /// An organization.
    Organization,
    /// A location.
    Location,
    /// An event.
    Event,
    /// A bounded concept (noun-phrase arguments only, §8 Stage 4).
    Concept,
    /// A product.
    Product,
}

impl EntityType {
    /// The `entity_type` discriminator — matches the §5 CHECK constraint.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            EntityType::Person => "PERSON",
            EntityType::Organization => "ORGANIZATION",
            EntityType::Location => "LOCATION",
            EntityType::Event => "EVENT",
            EntityType::Concept => "CONCEPT",
            EntityType::Product => "PRODUCT",
        }
    }
}

/// The closed predicate set for fact edges — mirrors the §5 CHECK constraint on
/// `triplets.predicate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Predicate {
    /// Spatial containment.
    LocatedIn,
    /// Mereological containment.
    PartOf,
    /// Authorship/creation.
    CreatedBy,
    /// Causation.
    Caused,
    /// Affected by.
    Affected,
    /// Participation in an event.
    ParticipatedIn,
    /// Weak association (the fallback relation).
    AssociatedWith,
    /// Produces / yields.
    Produces,
    /// Founded (org by person/org).
    Founded,
    /// Reliance on.
    DependsOn,
}

impl Predicate {
    /// The `predicate` discriminator — matches the §5 CHECK constraint.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Predicate::LocatedIn => "LOCATED_IN",
            Predicate::PartOf => "PART_OF",
            Predicate::CreatedBy => "CREATED_BY",
            Predicate::Caused => "CAUSED",
            Predicate::Affected => "AFFECTED",
            Predicate::ParticipatedIn => "PARTICIPATED_IN",
            Predicate::AssociatedWith => "ASSOCIATED_WITH",
            Predicate::Produces => "PRODUCES",
            Predicate::Founded => "FOUNDED",
            Predicate::DependsOn => "DEPENDS_ON",
        }
    }
}

impl std::str::FromStr for Predicate {
    type Err = KnowledgeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "LOCATED_IN" => Ok(Predicate::LocatedIn),
            "PART_OF" => Ok(Predicate::PartOf),
            "CREATED_BY" => Ok(Predicate::CreatedBy),
            "CAUSED" => Ok(Predicate::Caused),
            "AFFECTED" => Ok(Predicate::Affected),
            "PARTICIPATED_IN" => Ok(Predicate::ParticipatedIn),
            "ASSOCIATED_WITH" => Ok(Predicate::AssociatedWith),
            "PRODUCES" => Ok(Predicate::Produces),
            "FOUNDED" => Ok(Predicate::Founded),
            "DEPENDS_ON" => Ok(Predicate::DependsOn),
            other => Err(KnowledgeError::Backend(format!(
                "unknown predicate {other:?}"
            ))),
        }
    }
}

/// An entity record for the graph (§8 Stage 4). Identity is `entity_id` (a uuidv7
/// surrogate, stable for life per §7.8); the lookup key is
/// `UNIQUE(canonical_name, entity_type)`.
#[derive(Debug, Clone)]
pub struct EntityRecord {
    /// The stable surrogate id.
    pub entity_id: String,
    /// The canonical surface name.
    pub canonical_name: String,
    /// The supertype.
    pub entity_type: EntityType,
    /// Free-form refinement under the supertype, if any.
    pub subtype: Option<String>,
}

/// A fact edge between entities — the entity-level aggregation of `triplets`
/// (§8 Stage 4). Identity is `(subject_id, predicate, object_id)`; multiple chunks
/// asserting the same fact merge into one edge with `support_count`.
#[derive(Debug, Clone)]
pub struct Fact {
    /// Subject entity id.
    pub subject_id: String,
    /// The relation.
    pub predicate: Predicate,
    /// Object entity id.
    pub object_id: String,
    /// How many chunks assert this fact.
    pub support_count: u64,
    /// Edge properties JSON (`occurred_on`, `as_of`, …), if any.
    pub properties: Option<serde_json::Value>,
}

/// A KNN hit: the vector id and its similarity score.
#[derive(Debug, Clone)]
pub struct ScoredHit {
    /// The vector id that was upserted.
    pub id: String,
    /// Similarity score (higher = closer; cosine semantics per §4).
    pub score: f32,
}

/// Filter for filtered KNN (§9): results must satisfy it, though impls may
/// over-fetch and post-filter. Fields land with the retrieval build step (§15
/// step 5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkFilter {}

/// Knowledge-plane capability declaration (§9 rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KsCapabilities {
    /// Filtered ANN search is supported natively.
    pub filtered_ann: bool,
    /// Graph traversal (openCypher) is supported.
    pub graph_traversal: bool,
}

/// Knowledge-plane failures (§9). Every impl maps native errors into this
/// taxonomy; contract tests assert the retry classes.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    /// The store is not reachable or not ready (lock contention, closed handle).
    #[error("knowledge store unavailable: {0}")]
    Unavailable(String),
    /// A vector or graph operation failed inside the store.
    #[error("knowledge store operation failed: {0}")]
    Backend(String),
}

impl KnowledgeError {
    /// Retry class (§10): unavailability is transient; backend failures retry via
    /// `max_attempts` — genuinely corrupt data is a stage-layer `Fatal`, not an
    /// error variant (§10).
    #[must_use]
    pub fn class(&self) -> Class {
        Class::Retry
    }
}

/// The knowledge port (§9): HNSW vectors + property graph, `VectorSpace`-scoped.
///
/// Contracts (postconditions, not mechanisms):
/// - upserts are deterministic — replaying a stage is a no-op on written data (§7.1);
/// - after [`delete_doc`](KnowledgeStore::delete_doc), `knn` in *any* collection
///   never returns that doc's ids;
/// - filtered KNN results satisfy the filter (impls may over-fetch).
#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    /// Capability declaration.
    fn capabilities(&self) -> KsCapabilities;

    /// Deterministically upserts vectors into `space`, pairing `ids` with
    /// `vectors`. `doc_id` records which document each vector belongs to so
    /// [`delete_doc`](KnowledgeStore::delete_doc) can honor the delete→KNN
    /// postcondition (§7.6); non-document collections (entity names) pass `""`.
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn upsert_vectors(
        &self,
        space: VectorSpace,
        doc_id: &str,
        ids: &[&str],
        vectors: &[Vec<f32>],
    ) -> Result<(), KnowledgeError>;

    /// K-nearest-neighbor search in `space` under the optional filter.
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn knn(
        &self,
        space: VectorSpace,
        q: &[f32],
        k: usize,
        f: &ChunkFilter,
    ) -> Result<Vec<ScoredHit>, KnowledgeError>;

    /// Whether `id` currently has a vector in `space` — the boot-sweep check (§7.3).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn has_vector(&self, space: VectorSpace, id: &str) -> Result<bool, KnowledgeError>;

    /// Merges an entity node on its `UNIQUE(canonical_name, entity_type)` key —
    /// replay-idempotent (§7.1).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn upsert_entity(&self, e: &EntityRecord) -> Result<(), KnowledgeError>;

    /// `MERGE`s a `(:Chunk)-[:MENTIONS]->(:Entity)` edge — idempotent relinking (§8).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn link_mention(&self, chunk_id: &str, entity_id: &str) -> Result<(), KnowledgeError>;

    /// Folds `loser` away into `winner`: rewires `:MENTIONS` and fact edges, deletes
    /// the loser node (the Ladybug half of the §7.8 merge protocol).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn fold_entity(&self, loser: &str, winner: &str) -> Result<(), KnowledgeError>;

    /// Deletes every trace of `doc_id` — all vector collections plus the graph —
    /// so the delete→KNN postcondition holds everywhere (§9).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError>;

    /// Chunk ids connected to any of `ids` via `:MENTIONS` (the Stage 5 graph path).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn chunks_for_entities(&self, ids: &[&str]) -> Result<Vec<String>, KnowledgeError>;

    /// Facts within `hops` of any entity in `ids` (graph context for synthesis).
    ///
    /// # Errors
    /// [`KnowledgeError`] per its taxonomy.
    async fn facts_within_hops(&self, ids: &[&str], hops: u8) -> Result<Vec<Fact>, KnowledgeError>;
}
