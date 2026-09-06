//! openCypher graph operations on `LadybugDB`: entity `MERGE`s, `:MENTIONS`
//! edges, the §7.8 fold, and the traversal queries behind the
//! [`KnowledgeStore`](crate::knowledge::KnowledgeStore) port. All statements use
//! only primitives verified by the §2 build-gate probe (2026-09-06): parameterized
//! `CREATE`/`DELETE`, `MATCH`+`SET`, `BEGIN`/`COMMIT` on one connection, and
//! `DETACH DELETE`.

use crate::knowledge::{EntityRecord, EntityType, Fact, KnowledgeError, Predicate};
use lbug::Value;

use super::vectors::{LadybugStore, schema};
use schema::{backend, cypher_id_list, cypher_str, int_at, string_at};

pub(super) fn upsert_entity(store: &LadybugStore, e: &EntityRecord) -> Result<(), KnowledgeError> {
    let conn = store.conn()?;
    // Identity lookup is UNIQUE(canonical_name, entity_type) (§8 Stage 4); the
    // entity_id is stamped on create. Lookup-then-branch (not Cypher MERGE) —
    // the probe verified these primitives directly.
    let mut find = conn
        .prepare("MATCH (e:Entity {canonical_name: $n, entity_type: $t}) RETURN e.entity_id")
        .map_err(|e| backend(&e))?;
    let found = conn
        .execute(
            &mut find,
            vec![
                ("n", Value::String(e.canonical_name.clone())),
                ("t", Value::String(e.entity_type.as_str().into())),
            ],
        )
        .map_err(|e| backend(&e))?
        .next();
    if found.is_some() {
        let mut set = conn
            .prepare(
                "MATCH (e:Entity {canonical_name: $n, entity_type: $t}) \
                 SET e.subtype = $s",
            )
            .map_err(|e| backend(&e))?;
        conn.execute(
            &mut set,
            vec![
                ("n", Value::String(e.canonical_name.clone())),
                ("t", Value::String(e.entity_type.as_str().into())),
                ("s", Value::String(e.subtype.clone().unwrap_or_default())),
            ],
        )
        .map_err(|e| backend(&e))?;
    } else {
        let mut ins = conn
            .prepare(
                "CREATE (e:Entity {entity_id: $id, canonical_name: $n, \
                 entity_type: $t, subtype: $s})",
            )
            .map_err(|e| backend(&e))?;
        conn.execute(
            &mut ins,
            vec![
                ("id", Value::String(e.entity_id.clone())),
                ("n", Value::String(e.canonical_name.clone())),
                ("t", Value::String(e.entity_type.as_str().into())),
                ("s", Value::String(e.subtype.clone().unwrap_or_default())),
            ],
        )
        .map_err(|e| backend(&e))?;
    }
    Ok(())
}

pub(super) fn link_mention(
    store: &LadybugStore,
    chunk_id: &str,
    entity_id: &str,
) -> Result<(), KnowledgeError> {
    let conn = store.conn()?;
    // Both endpoints must exist; missing nodes are created as bare graph nodes
    // (the SQLite registry is the system of record, §3).
    let mut find_chunk = conn
        .prepare("MATCH (c:Chunk {chunk_id: $id}) RETURN count(c)")
        .map_err(|e| backend(&e))?;
    let n = conn
        .execute(
            &mut find_chunk,
            vec![("id", Value::String(chunk_id.into()))],
        )
        .map_err(|e| backend(&e))?
        .next()
        .map_or(0, |row| int_at(&row, 0));
    if n == 0 {
        conn.query(&format!(
            "CREATE (c:Chunk {{chunk_id: {}, doc_id: ''}})",
            cypher_str(chunk_id)
        ))
        .map_err(|e| backend(&e))?;
    }
    ensure_entity_node(&conn, entity_id)?;
    // Idempotent relink (§8): only create when the edge is absent.
    let mut find = conn
        .prepare(
            "MATCH (c:Chunk {chunk_id: $c})-[m:MENTIONS]->(e:Entity {entity_id: $e}) \
             RETURN count(m)",
        )
        .map_err(|e| backend(&e))?;
    let existing = conn
        .execute(
            &mut find,
            vec![
                ("c", Value::String(chunk_id.into())),
                ("e", Value::String(entity_id.into())),
            ],
        )
        .map_err(|e| backend(&e))?
        .next()
        .map_or(0, |row| int_at(&row, 0));
    if existing == 0 {
        conn.query(&format!(
            "MATCH (c:Chunk {{chunk_id: {}}}), (e:Entity {{entity_id: {}}}) \
             CREATE (c)-[:MENTIONS {{support: 1}}]->(e)",
            cypher_str(chunk_id),
            cypher_str(entity_id)
        ))
        .map_err(|e| backend(&e))?;
    }
    Ok(())
}

/// Creates a bare Entity node if absent (used by `link_mention` when the
/// registry write raced ahead of `upsert_entity`).
fn ensure_entity_node(conn: &lbug::Connection<'_>, entity_id: &str) -> Result<(), KnowledgeError> {
    let mut find = conn
        .prepare("MATCH (e:Entity {entity_id: $id}) RETURN count(e)")
        .map_err(|e| backend(&e))?;
    let n = conn
        .execute(&mut find, vec![("id", Value::String(entity_id.into()))])
        .map_err(|e| backend(&e))?
        .next()
        .map_or(0, |row| int_at(&row, 0));
    if n == 0 {
        conn.query(&format!(
            "CREATE (e:Entity {{entity_id: {}, canonical_name: '', entity_type: 'CONCEPT', subtype: ''}})",
            cypher_str(entity_id)
        ))
        .map_err(|e| backend(&e))?;
    }
    Ok(())
}

/// The §7.8 fold in ONE transaction (§2 gate (b), verified by probe): rewire
/// `:MENTIONS` and `FACT` edges from `loser` to `winner` preserving properties,
/// then delete the loser's edges and node.
pub(super) fn fold_entity(
    store: &LadybugStore,
    loser: &str,
    winner: &str,
) -> Result<(), KnowledgeError> {
    let conn = store.conn()?;
    let loser_lit = cypher_str(loser);
    let winner_lit = cypher_str(winner);
    conn.query("BEGIN TRANSACTION").map_err(|e| backend(&e))?;
    let steps = [
        // MENTIONS (Chunk→Entity): one step — the loser is always the target.
        format!(
            "MATCH (src:Chunk)-[m:MENTIONS]->(loser:Entity {{entity_id: {loser_lit}}}) \
             MATCH (winner:Entity {{entity_id: {winner_lit}}}) \
             CREATE (src)-[:MENTIONS {{support: m.support, properties: m.properties}}]->(winner)"
        ),
        // FACT: loser as subject.
        format!(
            "MATCH (loser:Entity {{entity_id: {loser_lit}}})-[f:FACT]->(obj:Entity) \
             MATCH (winner:Entity {{entity_id: {winner_lit}}}) \
             CREATE (winner)-[:FACT {{predicate: f.predicate, support: f.support, properties: f.properties}}]->(obj)"
        ),
        // FACT: loser as object.
        format!(
            "MATCH (subj:Entity)-[f:FACT]->(loser:Entity {{entity_id: {loser_lit}}}) \
             MATCH (winner:Entity {{entity_id: {winner_lit}}}) \
             CREATE (subj)-[:FACT {{predicate: f.predicate, support: f.support, properties: f.properties}}]->(winner)"
        ),
        // Directed deletes both ways — the binder refuses undirected rel deletes.
        format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})-[m:MENTIONS]->() DELETE m"),
        format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})<-[m:MENTIONS]-() DELETE m"),
        format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})-[f:FACT]->() DELETE f"),
        format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})<-[f:FACT]-() DELETE f"),
        format!("MATCH (loser:Entity {{entity_id: {loser_lit}}}) DELETE loser"),
    ];
    for step in &steps {
        if let Err(e) = conn.query(step) {
            let _ = conn.query("ROLLBACK");
            return Err(backend(&e));
        }
    }
    conn.query("COMMIT").map_err(|e| backend(&e))?;
    Ok(())
}

pub(super) fn chunks_for_entities(
    store: &LadybugStore,
    ids: &[&str],
) -> Result<Vec<String>, KnowledgeError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = store.conn()?;
    let res = conn
        .query(&format!(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) \
             WHERE e.entity_id IN {} \
             RETURN DISTINCT c.chunk_id",
            cypher_id_list(ids)
        ))
        .map_err(|e| backend(&e))?;
    Ok(res.map(|row| string_at(&row, 0)).collect())
}

/// Expands the fact graph hop by hop in Rust using single-hop directed
/// queries — deterministic, and built only from probe-verified primitives.
pub(super) fn facts_within_hops(
    store: &LadybugStore,
    ids: &[&str],
    hops: u8,
) -> Result<Vec<Fact>, KnowledgeError> {
    if ids.is_empty() || hops == 0 {
        return Ok(Vec::new());
    }
    let conn = store.conn()?;
    let mut facts: Vec<Fact> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut frontier: std::collections::HashSet<String> =
        ids.iter().map(|s| (*s).to_string()).collect();
    // Each hop expands one FACT edge from the current frontier in both
    // directions; facts found from later frontiers still report their true
    // endpoints.
    for _ in 0..hops {
        let mut next = std::collections::HashSet::new();
        for anchor in &frontier {
            for (subject, object) in [
                directed_facts(&conn, anchor, true)?,
                directed_facts(&conn, anchor, false)?,
            ]
            .into_iter()
            .flatten()
            {
                if seen.insert((subject.clone(), object.clone())) {
                    next.insert(subject.clone());
                    next.insert(object.clone());
                    facts.push(subject_and_object_to_fact(&conn, &subject, &object)?);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(facts)
}

/// Raw `(subject, object)` pairs one hop from `anchor`, outgoing or incoming.
fn directed_facts(
    conn: &lbug::Connection<'_>,
    anchor: &str,
    outgoing: bool,
) -> Result<Vec<(String, String)>, KnowledgeError> {
    let cypher = if outgoing {
        format!(
            "MATCH (a:Entity {{entity_id: {}}})-[f:FACT]->(b:Entity) RETURN DISTINCT a.entity_id, b.entity_id",
            cypher_str(anchor)
        )
    } else {
        format!(
            "MATCH (a:Entity)-[f:FACT]->(b:Entity {{entity_id: {}}}) RETURN DISTINCT a.entity_id, b.entity_id",
            cypher_str(anchor)
        )
    };
    let res = conn.query(&cypher).map_err(|e| backend(&e))?;
    Ok(res
        .map(|row| (string_at(&row, 0), string_at(&row, 1)))
        .collect())
}

/// Reads the FACT edge between a known `(subject, object)` pair.
fn subject_and_object_to_fact(
    conn: &lbug::Connection<'_>,
    subject: &str,
    object: &str,
) -> Result<Fact, KnowledgeError> {
    let mut stmt = conn
        .prepare(
            "MATCH (s:Entity {entity_id: $s})-[f:FACT]->(o:Entity {entity_id: $o}) \
             RETURN f.predicate, f.support, f.properties LIMIT 1",
        )
        .map_err(|e| backend(&e))?;
    let row = conn
        .execute(
            &mut stmt,
            vec![
                ("s", Value::String(subject.into())),
                ("o", Value::String(object.into())),
            ],
        )
        .map_err(|e| backend(&e))?
        .next()
        .ok_or_else(|| {
            KnowledgeError::Backend(format!("FACT edge vanished mid-read: {subject}->{object}"))
        })?;
    let predicate: Predicate = string_at(&row, 0).parse()?;
    let support = u64::try_from(int_at(&row, 1)).unwrap_or(0);
    let raw = string_at(&row, 2);
    let properties = if raw.is_empty() {
        None
    } else {
        serde_json::from_str(&raw).ok()
    };
    Ok(Fact {
        subject_id: subject.to_string(),
        predicate,
        object_id: object.to_string(),
        support_count: support,
        properties,
    })
}

/// `EntityType` from its §5 CHECK discriminator (graph round-trips).
impl std::str::FromStr for EntityType {
    type Err = KnowledgeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PERSON" => Ok(EntityType::Person),
            "ORGANIZATION" => Ok(EntityType::Organization),
            "LOCATION" => Ok(EntityType::Location),
            "EVENT" => Ok(EntityType::Event),
            "CONCEPT" => Ok(EntityType::Concept),
            "PRODUCT" => Ok(EntityType::Product),
            other => Err(KnowledgeError::Backend(format!(
                "unknown entity type {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::vectors::LadybugStore;
    use crate::knowledge::{
        ChunkFilter, EntityRecord, EntityType, KnowledgeStore, ModelId, VectorSpace,
    };

    fn entity(id: &str, name: &str, t: EntityType) -> EntityRecord {
        EntityRecord {
            entity_id: id.to_string(),
            canonical_name: name.to_string(),
            entity_type: t,
            subtype: None,
        }
    }

    #[tokio::test]
    async fn upsert_entity_round_trips_by_identity_key() {
        let store = LadybugStore::in_memory(4).unwrap();
        store
            .upsert_entity(&entity("e1", "Ada Lovelace", EntityType::Person))
            .await
            .unwrap();
        // Same (canonical_name, entity_type) → same node, new subtype.
        store
            .upsert_entity(&EntityRecord {
                entity_id: "e1".into(),
                canonical_name: "Ada Lovelace".into(),
                entity_type: EntityType::Person,
                subtype: Some("mathematician".into()),
            })
            .await
            .unwrap();
        // Same name, different type → different node (§8 Stage 4: Jordan rule).
        store
            .upsert_entity(&entity("e2", "Ada Lovelace", EntityType::Concept))
            .await
            .unwrap();
        let conn = store.conn().unwrap();
        let res = conn
            .query("MATCH (e:Entity) RETURN e.entity_id, e.subtype ORDER BY e.entity_id")
            .unwrap();
        let rows: Vec<Vec<lbug::Value>> = res.collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].to_string(), "e1");
        assert_eq!(rows[0][1].to_string(), "mathematician");
    }

    #[tokio::test]
    async fn fold_rewires_mentions_and_facts_in_one_transaction() {
        let store = LadybugStore::in_memory(4).unwrap();
        let conn = store.conn().unwrap();
        conn.query("CREATE (c:Chunk {chunk_id: 'c1', doc_id: 'd1'})")
            .unwrap();
        conn.query("CREATE (c:Chunk {chunk_id: 'c2', doc_id: 'd1'})")
            .unwrap();
        store
            .upsert_entity(&entity("loser", "Obama", EntityType::Person))
            .await
            .unwrap();
        store
            .upsert_entity(&entity("winner", "Barack Obama", EntityType::Person))
            .await
            .unwrap();
        store
            .upsert_entity(&entity("place", "Hawaii", EntityType::Location))
            .await
            .unwrap();
        conn.query("MATCH (c:Chunk {chunk_id: 'c1'}), (e:Entity {entity_id: 'loser'}) CREATE (c)-[:MENTIONS {support: 3}]->(e)").unwrap();
        conn.query("MATCH (c:Chunk {chunk_id: 'c2'}), (e:Entity {entity_id: 'loser'}) CREATE (c)-[:MENTIONS {support: 1}]->(e)").unwrap();
        conn.query("MATCH (a:Entity {entity_id: 'loser'}), (b:Entity {entity_id: 'place'}) CREATE (a)-[:FACT {predicate: 'LOCATED_IN', support: 2}]->(b)").unwrap();
        drop(conn);

        store.fold_entity("loser", "winner").await.unwrap();

        let conn = store.conn().unwrap();
        let mentions: Vec<Vec<lbug::Value>> = conn
            .query("MATCH (c:Chunk)-[m:MENTIONS]->(e:Entity) RETURN c.chunk_id, e.entity_id, m.support ORDER BY c.chunk_id")
            .unwrap()
            .collect();
        assert_eq!(mentions.len(), 2, "{mentions:?}");
        assert_eq!(mentions[0][1].to_string(), "winner");
        assert_eq!(mentions[0][2].to_string(), "3");
        let facts: Vec<Vec<lbug::Value>> = conn
            .query("MATCH (a:Entity)-[f:FACT]->(b:Entity) RETURN a.entity_id, b.entity_id, f.predicate")
            .unwrap()
            .collect();
        assert_eq!(facts.len(), 1, "{facts:?}");
        assert_eq!(facts[0][0].to_string(), "winner");
        assert_eq!(facts[0][2].to_string(), "LOCATED_IN");
        let losers: Vec<Vec<lbug::Value>> = conn
            .query("MATCH (e:Entity {entity_id: 'loser'}) RETURN e")
            .unwrap()
            .collect();
        assert!(losers.is_empty(), "loser node must be gone");
        // The loser entity's name-vector must also become unreachable — that's
        // the ER-side sweep's job, not fold's (§7.8); here only the graph folds.
        drop(store.knn(VectorSpace::EntityNames, &[0.0; 4], 1, &ChunkFilter {}));
    }

    #[tokio::test]
    async fn chunks_for_entities_returns_mentioning_chunks() {
        let store = LadybugStore::in_memory(4).unwrap();
        let conn = store.conn().unwrap();
        conn.query("CREATE (c:Chunk {chunk_id: 'ck1', doc_id: 'd'})")
            .unwrap();
        conn.query("CREATE (e:Entity {entity_id: 'x', canonical_name: 'X', entity_type: 'PERSON', subtype: ''})").unwrap();
        conn.query("MATCH (c:Chunk {chunk_id: 'ck1'}), (e:Entity {entity_id: 'x'}) CREATE (c)-[:MENTIONS]->(e)").unwrap();
        drop(conn);
        let chunks = store.chunks_for_entities(&["x"]).await.unwrap();
        assert_eq!(chunks, ["ck1"]);
        assert!(store.chunks_for_entities(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn facts_within_hops_walks_one_and_two_hops() {
        let store = LadybugStore::in_memory(4).unwrap();
        let conn = store.conn().unwrap();
        for id in ["a", "b", "c"] {
            conn.query(&format!(
                "CREATE (e:Entity {{entity_id: '{id}', canonical_name: '{id}', entity_type: 'PERSON', subtype: ''}})"
            ))
            .unwrap();
        }
        conn.query("MATCH (x:Entity {entity_id: 'a'}), (y:Entity {entity_id: 'b'}) CREATE (x)-[:FACT {predicate: 'CAUSED', support: 1}]->(y)").unwrap();
        conn.query("MATCH (x:Entity {entity_id: 'b'}), (y:Entity {entity_id: 'c'}) CREATE (x)-[:FACT {predicate: 'AFFECTED', support: 1}]->(y)").unwrap();
        drop(conn);

        let one = store.facts_within_hops(&["a"], 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].subject_id, "a");
        assert_eq!(one[0].object_id, "b");
        assert_eq!(one[0].support_count, 1);

        let two = store.facts_within_hops(&["a"], 2).await.unwrap();
        assert_eq!(two.len(), 2, "{two:?}");

        let none = store.facts_within_hops(&[], 2).await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn vector_collections_and_graph_share_the_store() {
        // §3: Ladybug holds both vector collections and the graph; the same
        // store instance serves both without interference.
        let store = LadybugStore::in_memory(4).unwrap();
        let space = VectorSpace::Chunks {
            model_id: ModelId::new("bge-small-en-v1.5"),
        };
        store
            .upsert_vectors(space.clone(), "doc1", &["ck1"], &[vec![1.0; 4]])
            .await
            .unwrap();
        store.fold_entity("nope", "also-nope").await.unwrap(); // no-op fold is fine
        store.delete_doc("doc1").await.unwrap();
        assert!(!store.has_vector(space, "ck1").await.unwrap());
    }
}
