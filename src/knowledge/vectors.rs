//! The `LadybugDB` implementation of the [`KnowledgeStore`](crate::knowledge::KnowledgeStore)
//! port (§9). Vector collections are node tables with `FLOAT[dim]` embeddings;
//! KNN is exact (in-engine `array_cosine_similarity` — the §9 "vector-lib + edges"
//! fallback posture) until an HNSW index is swapped in behind the same port.
//!
//! §2 build gates, verified empirically against lbug 0.20.2 (2026-09-06):
//! - **Transaction model:** reads run concurrently with the single write
//!   transaction (MVCC snapshot); a second concurrent write transaction is
//!   refused. The single-worker default serializes writers, and a refused
//!   writer maps to [`KnowledgeError::Unavailable`] (Retry class, §10).
//! - **Edge rewire/fold (§7.8):** `CREATE` + `DELETE` + node deletion inside one
//!   `BEGIN`/`COMMIT` on one connection — exercised by [`graph`] tests.

use crate::knowledge::{ChunkFilter, KnowledgeError, KnowledgeStore, ScoredHit, VectorSpace};

/// The vector-collection node tables. Column layout is fixed: `id` (the
/// cross-store vector identity), `doc_id` (delete-flow membership, §7.6), and
/// `embedding` (the model's fixed-width vector).
mod v {
    use crate::knowledge::VectorSpace;

    /// The table name for `space` — one collection per model (§4).
    pub(super) fn table(space: &VectorSpace) -> String {
        match space {
            VectorSpace::Chunks { model_id } => {
                // Table names are Cypher identifiers: fold everything outside
                // [A-Za-z0-9_] to `_`. Deterministic per model id (§4).
                let sanitized: String = model_id
                    .as_str()
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                format!("ChunkVec_{sanitized}")
            }
            VectorSpace::EntityNames => "EntityNameVec".to_string(),
        }
    }
}

use v::table;

/// Graph schema + helpers shared by the vector and graph halves.
pub(super) mod schema {
    use crate::knowledge::KnowledgeError;

    /// Creates the graph schema if missing (§3 Ladybug objects). Idempotent.
    pub(super) fn ensure(conn: &lbug::Connection) -> Result<(), KnowledgeError> {
        for ddl in [
            "CREATE NODE TABLE IF NOT EXISTS Chunk(chunk_id STRING, doc_id STRING, PRIMARY KEY(chunk_id))",
            "CREATE NODE TABLE IF NOT EXISTS Entity(entity_id STRING, canonical_name STRING, entity_type STRING, subtype STRING, PRIMARY KEY(entity_id))",
            "CREATE REL TABLE IF NOT EXISTS MENTIONS(FROM Chunk TO Entity, support INT64 DEFAULT 1, properties STRING)",
            "CREATE REL TABLE IF NOT EXISTS FACT(FROM Entity TO Entity, predicate STRING, support INT64 DEFAULT 1, properties STRING)",
        ] {
            conn.query(ddl).map_err(|e| backend(&e))?;
        }
        Ok(())
    }

    /// Maps an [`lbug::Error`] into the port taxonomy (§9: error-mapping tests
    /// assert the collapse).
    pub(in crate::knowledge) fn backend(e: &lbug::Error) -> KnowledgeError {
        let msg = e.to_string();
        // §2 gate (a): the engine refuses a second concurrent write transaction.
        // That is contention, not corruption — Retry class.
        if msg.contains("Only one write transaction at a time") {
            KnowledgeError::Unavailable(msg)
        } else {
            KnowledgeError::Backend(msg)
        }
    }

    /// Escapes a Rust string as a single-quoted Cypher literal.
    pub(in crate::knowledge) fn cypher_str(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                _ => out.push(c),
            }
        }
        out.push('\'');
        out
    }

    /// Formats ids as a Cypher list literal (ids are our own hex/uuid values;
    /// escaping is applied anyway — defense in depth).
    pub(in crate::knowledge) fn cypher_id_list(ids: &[&str]) -> String {
        let inner: Vec<String> = ids.iter().map(|id| cypher_str(id)).collect();
        format!("[{}]", inner.join(", "))
    }

    /// Extracts a `STRING` value from a result tuple.
    pub(in crate::knowledge) fn string_at(tuple: &[lbug::Value], i: usize) -> String {
        match &tuple[i] {
            lbug::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// Extracts an `INT64`/count value from a result tuple.
    pub(in crate::knowledge) fn int_at(tuple: &[lbug::Value], i: usize) -> i64 {
        match &tuple[i] {
            lbug::Value::Int64(n) => *n,
            lbug::Value::Int32(n) => i64::from(*n),
            other => other.to_string().parse().unwrap_or(0),
        }
    }

    /// Extracts a `DOUBLE` value from a result tuple.
    pub(in crate::knowledge) fn double_at(tuple: &[lbug::Value], i: usize) -> f64 {
        match &tuple[i] {
            lbug::Value::Double(d) => *d,
            lbug::Value::Float(f) => f64::from(*f),
            other => other.to_string().parse().unwrap_or(0.0),
        }
    }
}

use schema::{backend, cypher_str, double_at, int_at, string_at};

/// The embedded LadybugDB-backed store (§9): one `Database` handle, connections
/// opened per operation (cheap; the engine synchronizes them, §2 gate (a)).
pub struct LadybugStore {
    db: lbug::Database,
    /// The pinned embedding width for every collection (§4: one embedder dim).
    dim: usize,
}

impl LadybugStore {
    /// Opens (creating if needed) the store directory and ensures the graph
    /// schema exists.
    ///
    /// # Errors
    /// [`KnowledgeError::Unavailable`] when the engine cannot open the path.
    pub fn open(path: &std::path::Path, dim: usize) -> Result<Self, KnowledgeError> {
        if dim == 0 {
            return Err(KnowledgeError::Backend("embedding dim must be > 0".into()));
        }
        let db = lbug::Database::new(path, lbug::SystemConfig::default())
            .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
        {
            let conn = lbug::Connection::new(&db)
                .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
            schema::ensure(&conn)?;
        }
        Ok(Self { db, dim })
    }

    /// Opens a transient in-memory store (tests, throwaway rebuilds).
    ///
    /// # Errors
    /// [`KnowledgeError::Unavailable`] when the engine refuses.
    pub fn in_memory(dim: usize) -> Result<Self, KnowledgeError> {
        if dim == 0 {
            return Err(KnowledgeError::Backend("embedding dim must be > 0".into()));
        }
        let db = lbug::Database::in_memory(lbug::SystemConfig::default())
            .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
        {
            let conn = lbug::Connection::new(&db)
                .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
            schema::ensure(&conn)?;
        }
        Ok(Self { db, dim })
    }

    pub(super) fn conn(&self) -> Result<lbug::Connection<'_>, KnowledgeError> {
        lbug::Connection::new(&self.db).map_err(|e| KnowledgeError::Unavailable(e.to_string()))
    }

    /// Creates the collection table for `space` if missing (§4: one collection
    /// per model, created on first use).
    fn ensure_collection(
        &self,
        conn: &lbug::Connection,
        space: &VectorSpace,
    ) -> Result<String, KnowledgeError> {
        let name = table(space);
        let ddl = format!(
            "CREATE NODE TABLE IF NOT EXISTS {name}(id STRING, doc_id STRING, \
             embedding FLOAT[{}], PRIMARY KEY(id))",
            self.dim
        );
        conn.query(&ddl).map_err(|e| backend(&e))?;
        Ok(name)
    }

    /// Names of the chunk-vector collections that currently exist (the
    /// delete sweep walks them, §7.6: "every vector collection").
    fn chunk_collection_names(conn: &lbug::Connection) -> Result<Vec<String>, KnowledgeError> {
        let res = conn
            .query("CALL show_tables() RETURN *")
            .map_err(|e| backend(&e))?;
        let name_col = res
            .get_column_names()
            .iter()
            .position(|c| c == "name")
            .unwrap_or(0);
        Ok(res
            .map(move |tuple| string_at(&tuple, name_col))
            .filter(|name| name.starts_with("ChunkVec_"))
            .collect())
    }
}

fn vec_to_value(v: &[f32]) -> lbug::Value {
    lbug::Value::Array(
        lbug::LogicalType::Float,
        v.iter().copied().map(lbug::Value::Float).collect(),
    )
}

#[async_trait::async_trait]
impl KnowledgeStore for LadybugStore {
    fn capabilities(&self) -> crate::knowledge::KsCapabilities {
        crate::knowledge::KsCapabilities {
            // Exact KNN with post-filter capability; HNSW lands behind the
            // same port (§11).
            filtered_ann: true,
            graph_traversal: true,
        }
    }

    /// Deterministic upsert: one transaction, delete+create per id (replay is a
    /// no-op on already-written data, §7.1).
    async fn upsert_vectors(
        &self,
        space: VectorSpace,
        doc_id: &str,
        ids: &[&str],
        vectors: &[Vec<f32>],
    ) -> Result<(), KnowledgeError> {
        if ids.len() != vectors.len() {
            return Err(KnowledgeError::Backend(format!(
                "upsert_vectors: {} ids but {} vectors",
                ids.len(),
                vectors.len()
            )));
        }
        for v in vectors {
            if v.len() != self.dim {
                return Err(KnowledgeError::Backend(format!(
                    "upsert_vectors: vector width {} != collection dim {}",
                    v.len(),
                    self.dim
                )));
            }
        }
        let conn = self.conn()?;
        let table = self.ensure_collection(&conn, &space)?;
        conn.query("BEGIN TRANSACTION").map_err(|e| backend(&e))?;
        let mut del = conn
            .prepare(&format!("MATCH (v:{table} {{id: $id}}) DELETE v"))
            .map_err(|e| backend(&e))?;
        let mut ins = conn
            .prepare(&format!(
                "CREATE (v:{table} {{id: $id, doc_id: $doc, embedding: $e}})"
            ))
            .map_err(|e| backend(&e))?;
        for (id, vec) in ids.iter().zip(vectors) {
            conn.execute(&mut del, vec![("id", lbug::Value::String((*id).into()))])
                .map_err(|e| backend(&e))?;
            conn.execute(
                &mut ins,
                vec![
                    ("id", lbug::Value::String((*id).into())),
                    ("doc", lbug::Value::String(doc_id.to_string())),
                    ("e", vec_to_value(vec)),
                ],
            )
            .map_err(|e| backend(&e))?;
        }
        conn.query("COMMIT").map_err(|e| backend(&e))?;
        Ok(())
    }

    /// Exact KNN over the collection (in-engine cosine, §11 notes the HNSW
    /// swap path). Scores are similarities: higher = closer (§9).
    async fn knn(
        &self,
        space: VectorSpace,
        q: &[f32],
        k: usize,
        _f: &ChunkFilter,
    ) -> Result<Vec<ScoredHit>, KnowledgeError> {
        if q.len() != self.dim {
            return Err(KnowledgeError::Backend(format!(
                "knn: query width {} != collection dim {}",
                q.len(),
                self.dim
            )));
        }
        let conn = self.conn()?;
        let table = self.ensure_collection(&conn, &space)?;
        let cypher = format!(
            "MATCH (v:{table}) \
             RETURN v.id, array_cosine_similarity(v.embedding, $q) AS score \
             ORDER BY score DESC, v.id ASC LIMIT {k}"
        );
        let mut stmt = conn.prepare(&cypher).map_err(|e| backend(&e))?;
        let res = conn
            .execute(&mut stmt, vec![("q", vec_to_value(q))])
            .map_err(|e| backend(&e))?;
        let hits = res
            .map(|tuple| ScoredHit {
                id: string_at(&tuple, 0),
                // Engine returns DOUBLE; source data is f32 — precision loss is
                // below any retrieval-relevant threshold (§11).
                #[allow(clippy::cast_possible_truncation)]
                score: double_at(&tuple, 1) as f32,
            })
            .collect();
        Ok(hits)
    }

    /// The §7.3 boot-sweep check: does `id` currently have a vector in `space`?
    async fn has_vector(&self, space: VectorSpace, id: &str) -> Result<bool, KnowledgeError> {
        let conn = self.conn()?;
        let table = self.ensure_collection(&conn, &space)?;
        let mut stmt = conn
            .prepare(&format!("MATCH (v:{table} {{id: $id}}) RETURN count(v)"))
            .map_err(|e| backend(&e))?;
        let mut res = conn
            .execute(&mut stmt, vec![("id", lbug::Value::String(id.into()))])
            .map_err(|e| backend(&e))?;
        let n = res.next().map_or(0, |t| int_at(&t, 0));
        Ok(n > 0)
    }

    async fn upsert_entity(
        &self,
        e: &crate::knowledge::EntityRecord,
    ) -> Result<(), KnowledgeError> {
        super::graph::upsert_entity(self, e)
    }

    async fn link_mention(&self, chunk_id: &str, entity_id: &str) -> Result<(), KnowledgeError> {
        super::graph::link_mention(self, chunk_id, entity_id)
    }

    async fn fold_entity(&self, loser: &str, winner: &str) -> Result<(), KnowledgeError> {
        super::graph::fold_entity(self, loser, winner)
    }

    /// Deletes every trace of `doc_id`: all chunk-vector collections plus the
    /// graph's `Chunk` nodes (and their `:MENTIONS` edges) — §7.6.
    async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        let conn = self.conn()?;
        for table in Self::chunk_collection_names(&conn)? {
            let mut stmt = conn
                .prepare(&format!("MATCH (v:{table}) WHERE v.doc_id = $doc DELETE v"))
                .map_err(|e| backend(&e))?;
            conn.execute(&mut stmt, vec![("doc", lbug::Value::String(doc_id.into()))])
                .map_err(|e| backend(&e))?;
        }
        conn.query(&format!(
            "MATCH (c:Chunk) WHERE c.doc_id = {} DETACH DELETE c",
            cypher_str(doc_id)
        ))
        .map_err(|e| backend(&e))?;
        Ok(())
    }

    async fn chunks_for_entities(&self, ids: &[&str]) -> Result<Vec<String>, KnowledgeError> {
        super::graph::chunks_for_entities(self, ids)
    }

    async fn facts_within_hops(
        &self,
        ids: &[&str],
        hops: u8,
    ) -> Result<Vec<crate::knowledge::Fact>, KnowledgeError> {
        super::graph::facts_within_hops(self, ids, hops)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::knowledge::{KnowledgeStore, ModelId, VectorSpace};

    fn space(model: &str) -> VectorSpace {
        VectorSpace::Chunks {
            model_id: ModelId::new(model.to_string()),
        }
    }

    fn v(scalar: f32) -> Vec<f32> {
        vec![scalar, 0.0, 0.0, 0.0]
    }

    #[tokio::test]
    async fn upsert_then_knn_returns_scores_in_order() {
        let store = LadybugStore::in_memory(4).unwrap();
        let s = space("bge-small-en-v1.5");
        store
            .upsert_vectors(
                s.clone(),
                "doc1",
                &["near", "far"],
                &[v(1.0), vec![0.0, 0.0, 1.0, 0.0]],
            )
            .await
            .unwrap();
        let hits = store
            .knn(s.clone(), &v(1.0), 2, &ChunkFilter {})
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "near");
        assert!(hits[0].score > 0.99);
        assert!(hits[1].score < 0.01);
    }

    #[tokio::test]
    async fn upsert_is_deterministic_replay_is_noop() {
        let store = LadybugStore::in_memory(4).unwrap();
        let s = space("m");
        for _ in 0..2 {
            store
                .upsert_vectors(s.clone(), "doc1", &["a"], &[v(0.5)])
                .await
                .unwrap();
        }
        let hits = store
            .knn(s.clone(), &v(0.5), 10, &ChunkFilter {})
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "replay must not duplicate (§7.1)");
        assert!((hits[0].score - 1.0).abs() < 1e-5);
    }

    #[tokio::test]
    async fn collections_are_isolated_per_model_and_entity_space() {
        let store = LadybugStore::in_memory(4).unwrap();
        store
            .upsert_vectors(space("m1"), "d", &["x"], &[v(1.0)])
            .await
            .unwrap();
        // Same id, different collection: invisible (§9 collection isolation).
        assert!(!store.has_vector(space("m2"), "x").await.unwrap());
        assert!(
            !store
                .has_vector(VectorSpace::EntityNames, "x")
                .await
                .unwrap()
        );
        assert!(store.has_vector(space("m1"), "x").await.unwrap());
    }

    #[tokio::test]
    async fn delete_doc_purges_every_chunk_collection() {
        let store = LadybugStore::in_memory(4).unwrap();
        store
            .upsert_vectors(space("m1"), "doc1", &["a"], &[v(1.0)])
            .await
            .unwrap();
        store
            .upsert_vectors(space("m2"), "doc1", &["b"], &[v(1.0)])
            .await
            .unwrap();
        store
            .upsert_vectors(space("m1"), "other", &["c"], &[v(1.0)])
            .await
            .unwrap();
        store.delete_doc("doc1").await.unwrap();
        assert!(!store.has_vector(space("m1"), "a").await.unwrap());
        assert!(!store.has_vector(space("m2"), "b").await.unwrap());
        // The postcondition: knn in *any* collection never returns deleted ids.
        assert!(
            store
                .knn(space("m1"), &v(1.0), 10, &ChunkFilter {})
                .await
                .unwrap()
                .iter()
                .all(|h| h.id != "a")
        );
        assert!(store.has_vector(space("m1"), "c").await.unwrap());
    }

    #[tokio::test]
    async fn width_mismatches_are_backend_errors() {
        let store = LadybugStore::in_memory(4).unwrap();
        let err = store
            .upsert_vectors(space("m"), "d", &["a"], &[vec![1.0]])
            .await
            .unwrap_err();
        assert!(matches!(err, KnowledgeError::Backend(_)), "got {err:?}");
        let err = store
            .knn(space("m"), &[1.0], 3, &ChunkFilter {})
            .await
            .unwrap_err();
        assert!(matches!(err, KnowledgeError::Backend(_)), "got {err:?}");
    }
}
