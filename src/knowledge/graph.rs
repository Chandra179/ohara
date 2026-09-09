//! openCypher graph operations on `LadybugDB`: entity `MERGE`s, `:MENTIONS`
//! edges, the §7.8 fold, and the traversal queries behind the
//! [`KnowledgeStore`](crate::knowledge::KnowledgeStore) port. All statements use
//! only primitives verified by the §2 build-gate probe (2026-09-06): parameterized
//! `CREATE`/`DELETE`, `MATCH`+`SET`, `BEGIN`/`COMMIT` on one connection, and
//! `DETACH DELETE`.

use crate::knowledge::{EntityRecord, Fact, FactCaps, KnowledgeError, Predicate};
use lbug::Value;

use super::vectors::{LadybugStore, schema};
use schema::{backend, cypher_id_list, cypher_str, int_at, optional_string_at, string_at};

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
        .map_or(Ok(0), |row| int_at(&row, 0))?;
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
        .map_or(Ok(0), |row| int_at(&row, 0))?;
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
        .map_or(Ok(0), |row| int_at(&row, 0))?;
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
    if loser == winner {
        return Ok(());
    }
    let conn = store.conn()?;
    let loser_exists = entity_exists(&conn, loser)?;
    if !loser_exists {
        return Ok(());
    }
    if !entity_exists(&conn, winner)? {
        return Err(KnowledgeError::Backend(format!(
            "fold_entity: winner {winner} does not exist"
        )));
    }

    let loser_lit = cypher_str(loser);
    conn.query("BEGIN TRANSACTION").map_err(|e| backend(&e))?;
    let result: Result<(), KnowledgeError> = (|| {
        let mentions = read_mentions(&conn, loser)?;
        let facts = read_fact_transfers(&conn, loser, winner)?;
        for mention in &mentions {
            rewire_mention(&conn, mention, winner)?;
        }
        for fact in &facts {
            rewire_fact(&conn, fact)?;
        }

        // Directed deletes both ways — the binder refuses undirected rel deletes.
        let steps = [
            format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})-[m:MENTIONS]->() DELETE m"),
            format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})<-[m:MENTIONS]-() DELETE m"),
            format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})-[f:FACT]->() DELETE f"),
            format!("MATCH (loser:Entity {{entity_id: {loser_lit}}})<-[f:FACT]-() DELETE f"),
            format!("MATCH (loser:Entity {{entity_id: {loser_lit}}}) DELETE loser"),
        ];
        for step in &steps {
            conn.query(step).map_err(|e| backend(&e))?;
        }
        Ok(())
    })();
    if result.is_ok() {
        conn.query("COMMIT").map_err(|e| backend(&e))?;
        Ok(())
    } else {
        let _ = conn.query("ROLLBACK");
        result
    }
}

#[derive(Debug)]
struct MentionTransfer {
    chunk_id: String,
    support: i64,
    properties: Option<String>,
}

#[derive(Debug)]
struct FactTransfer {
    subject_id: String,
    predicate: String,
    object_id: String,
    support: i64,
    properties: Option<String>,
}

fn entity_exists(conn: &lbug::Connection<'_>, entity_id: &str) -> Result<bool, KnowledgeError> {
    let mut find = conn
        .prepare("MATCH (e:Entity {entity_id: $id}) RETURN count(e)")
        .map_err(|e| backend(&e))?;
    let count = conn
        .execute(&mut find, vec![("id", Value::String(entity_id.into()))])
        .map_err(|e| backend(&e))?
        .next()
        .map_or(Ok(0), |row| int_at(&row, 0))?;
    Ok(count > 0)
}

fn read_mentions(
    conn: &lbug::Connection<'_>,
    loser: &str,
) -> Result<Vec<MentionTransfer>, KnowledgeError> {
    let rows = conn
        .query(&format!(
            "MATCH (src:Chunk)-[m:MENTIONS]->(loser:Entity {{entity_id: {}}}) \
             RETURN src.chunk_id, m.support, m.properties",
            cypher_str(loser)
        ))
        .map_err(|e| backend(&e))?;
    rows.map(|row| {
        Ok(MentionTransfer {
            chunk_id: string_at(&row, 0)?,
            support: int_at(&row, 1)?,
            properties: optional_string_at(&row, 2)?,
        })
    })
    .collect()
}

fn read_fact_transfers(
    conn: &lbug::Connection<'_>,
    loser: &str,
    winner: &str,
) -> Result<Vec<FactTransfer>, KnowledgeError> {
    let mut transfers = std::collections::HashMap::<(String, String, String), FactTransfer>::new();
    for (query, loser_is_subject) in [
        (
            format!(
                "MATCH (loser:Entity {{entity_id: {}}})-[f:FACT]->(other:Entity) \
                 RETURN other.entity_id, f.predicate, f.support, f.properties",
                cypher_str(loser)
            ),
            true,
        ),
        (
            format!(
                "MATCH (other:Entity)-[f:FACT]->(loser:Entity {{entity_id: {}}}) \
                 RETURN other.entity_id, f.predicate, f.support, f.properties",
                cypher_str(loser)
            ),
            false,
        ),
    ] {
        let rows = conn.query(&query).map_err(|e| backend(&e))?;
        for row in rows {
            let other = string_at(&row, 0)?;
            let predicate = string_at(&row, 1)?;
            let support = int_at(&row, 2)?;
            let properties = optional_string_at(&row, 3)?;
            let (subject_id, object_id) = if loser_is_subject {
                (winner.to_string(), other)
            } else {
                (other, winner.to_string())
            };
            let key = (subject_id.clone(), predicate.clone(), object_id.clone());
            if let Some(existing) = transfers.get_mut(&key) {
                existing.support = existing.support.saturating_add(support);
                existing.properties = Some(merge_edge_properties(
                    existing.properties.as_deref(),
                    properties.as_deref(),
                ));
            } else {
                transfers.insert(
                    key,
                    FactTransfer {
                        subject_id,
                        predicate,
                        object_id,
                        support,
                        properties,
                    },
                );
            }
        }
    }
    let mut transfers: Vec<_> = transfers.into_values().collect();
    transfers.sort_by(|a, b| {
        (&a.subject_id, &a.predicate, &a.object_id).cmp(&(
            &b.subject_id,
            &b.predicate,
            &b.object_id,
        ))
    });
    Ok(transfers)
}

fn rewire_mention(
    conn: &lbug::Connection<'_>,
    mention: &MentionTransfer,
    winner: &str,
) -> Result<(), KnowledgeError> {
    let mut find = conn
        .prepare(
            "MATCH (src:Chunk {chunk_id: $chunk})-[m:MENTIONS]->(winner:Entity {entity_id: $winner}) \
             RETURN m.support, m.properties",
        )
        .map_err(|e| backend(&e))?;
    let existing = conn
        .execute(
            &mut find,
            vec![
                ("chunk", Value::String(mention.chunk_id.clone())),
                ("winner", Value::String(winner.into())),
            ],
        )
        .map_err(|e| backend(&e))?
        .next();
    let has_existing = existing.is_some();
    let (support, properties) = if let Some(row) = existing {
        (
            int_at(&row, 0)?.saturating_add(mention.support),
            optional_string_at(&row, 1)?.or_else(|| mention.properties.clone()),
        )
    } else {
        (mention.support, mention.properties.clone())
    };
    let properties_value = properties.as_deref().map_or_else(
        || Value::Null(lbug::LogicalType::String),
        |value| Value::String(value.into()),
    );
    if has_existing {
        let mut set = conn
            .prepare(
                "MATCH (src:Chunk {chunk_id: $chunk})-[m:MENTIONS]->(winner:Entity {entity_id: $winner}) \
                 SET m.support = $support, m.properties = $properties",
            )
            .map_err(|e| backend(&e))?;
        conn.execute(
            &mut set,
            vec![
                ("chunk", Value::String(mention.chunk_id.clone())),
                ("winner", Value::String(winner.into())),
                ("support", Value::Int64(support)),
                ("properties", properties_value),
            ],
        )
        .map_err(|e| backend(&e))?;
    } else {
        conn.query(&format!(
            "MATCH (src:Chunk {{chunk_id: {}}}), (winner:Entity {{entity_id: {}}}) \
             CREATE (src)-[:MENTIONS {{support: {}, properties: {}}}]->(winner)",
            cypher_str(&mention.chunk_id),
            cypher_str(winner),
            support,
            properties
                .as_deref()
                .map_or_else(|| "NULL".to_string(), cypher_str)
        ))
        .map_err(|e| backend(&e))?;
    }
    Ok(())
}

fn rewire_fact(conn: &lbug::Connection<'_>, fact: &FactTransfer) -> Result<(), KnowledgeError> {
    let mut find = conn
        .prepare(
            "MATCH (subject:Entity {entity_id: $subject})-[f:FACT {predicate: $predicate}]->(object:Entity {entity_id: $object}) \
             RETURN f.support, f.properties",
        )
        .map_err(|e| backend(&e))?;
    let existing = conn
        .execute(
            &mut find,
            vec![
                ("subject", Value::String(fact.subject_id.clone())),
                ("predicate", Value::String(fact.predicate.clone())),
                ("object", Value::String(fact.object_id.clone())),
            ],
        )
        .map_err(|e| backend(&e))?
        .next();
    let has_existing = existing.is_some();
    let (support, properties) = if let Some(row) = existing {
        (
            int_at(&row, 0)?.saturating_add(fact.support),
            Some(merge_edge_properties(
                optional_string_at(&row, 1)?.as_deref(),
                fact.properties.as_deref(),
            )),
        )
    } else {
        (
            fact.support,
            Some(merge_edge_properties(None, fact.properties.as_deref())),
        )
    };
    let mut set_or_create = if has_existing {
        conn.prepare(
            "MATCH (subject:Entity {entity_id: $subject})-[f:FACT {predicate: $predicate}]->(object:Entity {entity_id: $object}) \
             SET f.support = $support, f.properties = $properties",
        )
        .map_err(|e| backend(&e))?
    } else {
        conn.prepare(
            "MATCH (subject:Entity {entity_id: $subject}), (object:Entity {entity_id: $object}) \
             CREATE (subject)-[:FACT {predicate: $predicate, support: $support, properties: $properties}]->(object)",
        )
        .map_err(|e| backend(&e))?
    };
    conn.execute(
        &mut set_or_create,
        vec![
            ("subject", Value::String(fact.subject_id.clone())),
            ("predicate", Value::String(fact.predicate.clone())),
            ("object", Value::String(fact.object_id.clone())),
            ("support", Value::Int64(support)),
            ("properties", Value::String(properties.unwrap_or_default())),
        ],
    )
    .map_err(|e| backend(&e))?;
    Ok(())
}

/// Reserved top-level keys of the edge `properties` JSON — the aggregation
/// state ([`merge_fact`]) owns them; caller properties named identically are
/// superseded (`occurred_on` feeds the occurrences list instead).
const EVIDENCE_KEY: &str = "evidence";
const OCCURRENCES_KEY: &str = "occurrences";

/// Parses the aggregation state out of an edge's `properties` JSON: the capped
/// evidence chunk-id list, the capped occurrences list, and the caller's own
/// properties (with [`OCCURRENCES_KEY`]/[`EVIDENCE_KEY`] removed). Absent or
/// malformed JSON yields empty state — a stored blob is derived data (§7.9), and
/// the next merge rewrites it deterministically.
fn edge_state(raw: &str) -> (Vec<String>, Vec<String>, serde_json::Value) {
    let mut parsed: serde_json::Value = serde_json::from_str(raw)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
    let evidence = string_list(parsed.get(EVIDENCE_KEY));
    let occurrences = string_list(parsed.get(OCCURRENCES_KEY));
    if let serde_json::Value::Object(map) = &mut parsed {
        map.remove(EVIDENCE_KEY);
        map.remove(OCCURRENCES_KEY);
        map.remove("occurred_on");
    }
    (evidence, occurrences, parsed)
}

/// Reads a JSON array of strings; everything else reads as empty.
fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map_or_else(Vec::new, |arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
}

/// Pushes `item` onto `list` (dedup; capped at `max`). Returns whether the list
/// gained a new entry.
fn push_capped(list: &mut Vec<String>, item: String, max: usize) -> bool {
    if max == 0 || list.iter().any(|existing| existing == &item) {
        return false;
    }
    if list.len() < max {
        list.push(item);
    }
    true
}

/// Merges `props` (a triplet's properties: `occurred_on`, `as_of`, …) into the
/// edge's stored state: `occurred_on` values union into `occurrences` (capped,
/// deduped, returns the new ones) and `as_of` keeps the latest value.
fn merge_properties(
    occurrences: &mut Vec<String>,
    stored: &mut serde_json::Value,
    props: Option<&serde_json::Value>,
    caps: FactCaps,
) {
    let Some(props) = props else {
        return;
    };
    let Some(obj) = props.as_object() else {
        return;
    };
    if let Some(occurred_on) = obj.get("occurred_on") {
        let values: Vec<String> = occurred_on
            .as_array()
            .map_or_else(Vec::new, |arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .into_iter()
            .chain(occurred_on.as_str().map(str::to_string))
            .collect();
        for value in values {
            push_capped(occurrences, value, caps.max_occurrences);
        }
    }
    // `as_of` keeps the latest: ISO-8601 timestamps compare correctly
    // lexicographically (§8 Stage 4).
    if let Some(as_of) = obj.get("as_of").and_then(serde_json::Value::as_str) {
        let current = stored
            .get("as_of")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if as_of > current
            && let serde_json::Value::Object(map) = stored
        {
            map.insert(
                "as_of".to_string(),
                serde_json::Value::String(as_of.to_string()),
            );
        }
    }
}

/// Serializes the aggregation state into the edge's `properties` JSON.
fn edge_json(evidence: &[String], occurrences: &[String], caller: &serde_json::Value) -> String {
    let mut object = serde_json::Map::new();
    if let serde_json::Value::Object(map) = caller {
        object.extend(map.clone());
    }
    object.insert(
        EVIDENCE_KEY.to_string(),
        serde_json::Value::Array(
            evidence
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    object.insert(
        OCCURRENCES_KEY.to_string(),
        serde_json::Value::Array(
            occurrences
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    serde_json::Value::Object(object).to_string()
}

/// Unions two stored fact-property blobs while keeping the aggregation keys
/// under the store's control. Folds have no per-call caps, so the existing
/// bounded values are retained and newly discovered values are appended once.
fn merge_edge_properties(left: Option<&str>, right: Option<&str>) -> String {
    let (mut evidence, mut occurrences, mut caller) = edge_state(left.unwrap_or(""));
    let (right_evidence, right_occurrences, right_caller) = edge_state(right.unwrap_or(""));
    for item in right_evidence {
        push_capped(&mut evidence, item, usize::MAX);
    }
    for item in right_occurrences {
        push_capped(&mut occurrences, item, usize::MAX);
    }
    if let (serde_json::Value::Object(left), serde_json::Value::Object(right)) =
        (&mut caller, right_caller)
    {
        for (key, value) in right {
            if key == "as_of" {
                let current = left
                    .get(&key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if value.as_str().is_some_and(|candidate| candidate > current) {
                    left.insert(key, value);
                }
            } else {
                left.entry(key).or_insert(value);
            }
        }
    }
    edge_json(&evidence, &occurrences, &caller)
}

/// Merges one chunk's assertion into the fact edge `(subject, predicate, object)`
/// (§8 Stage 4 aggregation): creates the edge if absent; otherwise increments
/// `support_count` iff `evidence_chunk` is new, and unions `occurrences` — both
/// capped lists, replay with the same inputs a no-op (§7.1). One transaction
/// (read-modify-write atomicity under the single-writer regime, §2 gate (a)).
pub(super) fn merge_fact(
    store: &LadybugStore,
    subject_id: &str,
    predicate: Predicate,
    object_id: &str,
    evidence_chunk: &str,
    properties: Option<&serde_json::Value>,
    caps: FactCaps,
) -> Result<(), KnowledgeError> {
    let conn = store.conn()?;
    // Both endpoints must exist — callers upsert entities before facts (§7.2
    // intent-before-write); a missing endpoint is a broken invariant upstream.
    for id in [subject_id, object_id] {
        let mut find = conn
            .prepare("MATCH (e:Entity {entity_id: $id}) RETURN count(e)")
            .map_err(|e| backend(&e))?;
        let n = conn
            .execute(&mut find, vec![("id", Value::String(id.into()))])
            .map_err(|e| backend(&e))?
            .next()
            .map_or(Ok(0), |row| int_at(&row, 0))?;
        if n == 0 {
            return Err(KnowledgeError::Backend(format!(
                "merge_fact: entity {id} does not exist"
            )));
        }
    }

    conn.query("BEGIN TRANSACTION").map_err(|e| backend(&e))?;
    let result: Result<(), KnowledgeError> = (|| {
        let mut read = conn
            .prepare(
                "MATCH (s:Entity {entity_id: $s})-[f:FACT {predicate: $p}]->(o:Entity {entity_id: $o}) \
                 RETURN f.support, f.properties",
            )
            .map_err(|e| backend(&e))?;
        let row = conn
            .execute(
                &mut read,
                vec![
                    ("s", Value::String(subject_id.into())),
                    ("p", Value::String(predicate.as_str().into())),
                    ("o", Value::String(object_id.into())),
                ],
            )
            .map_err(|e| backend(&e))?
            .next();
        if let Some(row) = row {
            let support = u64::try_from(int_at(&row, 0)?).map_err(|_| {
                KnowledgeError::Backend("FACT support count cannot be negative".to_string())
            })?;
            let raw = optional_string_at(&row, 1)?.unwrap_or_default();
            let (mut evidence, mut occurrences, mut caller) = edge_state(&raw);
            let gained = push_capped(&mut evidence, evidence_chunk.to_string(), caps.max_evidence);
            merge_properties(&mut occurrences, &mut caller, properties, caps);
            let support = support + u64::from(gained);
            let merged = edge_json(&evidence, &occurrences, &caller);
            let mut set = conn
                .prepare(
                    "MATCH (s:Entity {entity_id: $s})-[f:FACT {predicate: $p}]->(o:Entity {entity_id: $o}) \
                     SET f.support = $sup, f.properties = $props",
                )
                .map_err(|e| backend(&e))?;
            conn.execute(
                &mut set,
                vec![
                    ("s", Value::String(subject_id.into())),
                    ("p", Value::String(predicate.as_str().into())),
                    ("o", Value::String(object_id.into())),
                    (
                        "sup",
                        Value::Int64(i64::try_from(support).unwrap_or(i64::MAX)),
                    ),
                    ("props", Value::String(merged)),
                ],
            )
            .map_err(|e| backend(&e))?;
        } else {
            let (mut evidence, mut occurrences, mut caller) = edge_state("");
            let _ = push_capped(&mut evidence, evidence_chunk.to_string(), caps.max_evidence);
            merge_properties(&mut occurrences, &mut caller, properties, caps);
            let merged = edge_json(&evidence, &occurrences, &caller);
            let mut create = conn
                .prepare(
                    "MATCH (s:Entity {entity_id: $s}), (o:Entity {entity_id: $o}) \
                     CREATE (s)-[f:FACT {predicate: $p, support: $sup, properties: $props}]->(o)",
                )
                .map_err(|e| backend(&e))?;
            conn.execute(
                &mut create,
                vec![
                    ("s", Value::String(subject_id.into())),
                    ("o", Value::String(object_id.into())),
                    ("p", Value::String(predicate.as_str().into())),
                    ("sup", Value::Int64(1)),
                    ("props", Value::String(merged)),
                ],
            )
            .map_err(|e| backend(&e))?;
        }
        Ok(())
    })();
    if result.is_ok() {
        conn.query("COMMIT").map_err(|e| backend(&e))?;
        Ok(())
    } else {
        let _ = conn.query("ROLLBACK");
        result
    }
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
    res.map(|row| string_at(&row, 0))
        .collect::<Result<Vec<_>, _>>()
}

/// Expands the fact graph hop by hop in Rust using single-hop directed
/// queries — deterministic, and built only from probe-verified primitives.
/// Facts are deduped by their §8 identity `(subject, predicate, object)` — two
/// edges between the same entities under different predicates are different
/// facts.
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
            for (subject, object, predicate) in [
                directed_facts(&conn, anchor, true)?,
                directed_facts(&conn, anchor, false)?,
            ]
            .into_iter()
            .flatten()
            {
                if seen.insert((subject.clone(), predicate.clone(), object.clone())) {
                    next.insert(subject.clone());
                    next.insert(object.clone());
                    facts.push(subject_and_object_to_fact(
                        &conn, &subject, &predicate, &object,
                    )?);
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

/// Raw `(subject, object, predicate)` edges one hop from `anchor`, outgoing or
/// incoming.
fn directed_facts(
    conn: &lbug::Connection<'_>,
    anchor: &str,
    outgoing: bool,
) -> Result<Vec<(String, String, String)>, KnowledgeError> {
    let cypher = if outgoing {
        format!(
            "MATCH (a:Entity {{entity_id: {}}})-[f:FACT]->(b:Entity) RETURN DISTINCT a.entity_id, b.entity_id, f.predicate",
            cypher_str(anchor)
        )
    } else {
        format!(
            "MATCH (a:Entity)-[f:FACT]->(b:Entity {{entity_id: {}}}) RETURN DISTINCT a.entity_id, b.entity_id, f.predicate",
            cypher_str(anchor)
        )
    };
    let res = conn.query(&cypher).map_err(|e| backend(&e))?;
    res.map(|row| {
        Ok((
            string_at(&row, 0)?,
            string_at(&row, 1)?,
            string_at(&row, 2)?,
        ))
    })
    .collect::<Result<Vec<_>, KnowledgeError>>()
}

/// Reads the FACT edge between a known `(subject, predicate, object)` triple —
/// the §8 fact identity.
fn subject_and_object_to_fact(
    conn: &lbug::Connection<'_>,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Result<Fact, KnowledgeError> {
    let mut stmt = conn
        .prepare(
            "MATCH (s:Entity {entity_id: $s})-[f:FACT {predicate: $p}]->(o:Entity {entity_id: $o}) \
             RETURN f.support, f.properties LIMIT 1",
        )
        .map_err(|e| backend(&e))?;
    let row = conn
        .execute(
            &mut stmt,
            vec![
                ("s", Value::String(subject.into())),
                ("p", Value::String(predicate.into())),
                ("o", Value::String(object.into())),
            ],
        )
        .map_err(|e| backend(&e))?
        .next()
        .ok_or_else(|| {
            KnowledgeError::Backend(format!(
                "FACT edge vanished mid-read: {subject}--{predicate}-->{object}"
            ))
        })?;
    let parsed_predicate: Predicate = predicate.parse()?;
    let support = u64::try_from(int_at(&row, 0)?).map_err(|_| {
        KnowledgeError::Backend("FACT support count cannot be negative".to_string())
    })?;
    let raw = optional_string_at(&row, 1)?.unwrap_or_default();
    let properties = if raw.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&raw).map_err(|error| {
            KnowledgeError::Backend(format!("invalid FACT properties JSON: {error}"))
        })?)
    };
    Ok(Fact {
        subject_id: subject.to_string(),
        predicate: parsed_predicate,
        object_id: object.to_string(),
        support_count: support,
        properties,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::vectors::LadybugStore;
    use crate::knowledge::{
        ChunkFilter, EntityRecord, EntityType, KnowledgeError, KnowledgeStore, ModelId, Predicate,
        VectorSpace,
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

    #[tokio::test]
    async fn merge_fact_creates_and_aggregates_by_edge_identity() {
        let store = LadybugStore::in_memory(4).unwrap();
        store
            .upsert_entity(&entity("s", "SQLite", EntityType::Product))
            .await
            .unwrap();
        store
            .upsert_entity(&entity("o", "C", EntityType::Concept))
            .await
            .unwrap();
        let caps = crate::knowledge::FactCaps {
            max_evidence: 8,
            max_occurrences: 8,
        };

        // First assertion creates the edge with support 1.
        store
            .merge_fact("s", Predicate::DependsOn, "o", "ck1", None, caps)
            .await
            .unwrap();
        // §7.1 replay: the same chunk does not double-count.
        store
            .merge_fact("s", Predicate::DependsOn, "o", "ck1", None, caps)
            .await
            .unwrap();
        // A second chunk's assertion increments support and evidence.
        store
            .merge_fact("s", Predicate::DependsOn, "o", "ck2", None, caps)
            .await
            .unwrap();

        let facts = store.facts_within_hops(&["s"], 1).await.unwrap();
        assert_eq!(facts.len(), 1, "{facts:?}");
        assert_eq!(facts[0].subject_id, "s");
        assert_eq!(facts[0].predicate, Predicate::DependsOn);
        assert_eq!(facts[0].object_id, "o");
        assert_eq!(facts[0].support_count, 2, "replay must not double-count");
        let properties = facts[0].properties.as_ref().unwrap();
        assert_eq!(
            properties
                .get("evidence")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
            2,
            "{properties:?}"
        );

        // A different predicate is a different edge by identity.
        store
            .merge_fact("s", Predicate::AssociatedWith, "o", "ck1", None, caps)
            .await
            .unwrap();
        assert_eq!(store.facts_within_hops(&["s"], 1).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn merge_fact_caps_evidence_and_occurrences_and_tracks_as_of() {
        let store = LadybugStore::in_memory(4).unwrap();
        store
            .upsert_entity(&entity("s", "X", EntityType::Concept))
            .await
            .unwrap();
        store
            .upsert_entity(&entity("o", "Y", EntityType::Concept))
            .await
            .unwrap();
        let caps = crate::knowledge::FactCaps {
            max_evidence: 2,
            max_occurrences: 2,
        };
        for i in 0..5 {
            let chunk = format!("ck{i}");
            let props = serde_json::json!({"occurred_on": format!("2020-0{i}-01")});
            store
                .merge_fact(
                    "s",
                    Predicate::AssociatedWith,
                    "o",
                    &chunk,
                    Some(&props),
                    caps,
                )
                .await
                .unwrap();
        }
        let facts = store.facts_within_hops(&["s"], 1).await.unwrap();
        let properties = facts[0].properties.as_ref().unwrap();
        let evidence = properties
            .get("evidence")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let occurrences = properties
            .get("occurrences")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(evidence.len(), 2, "evidence is capped: {properties:?}");
        assert_eq!(occurrences.len(), 2, "occurrences are capped");
        // The count still reflects every distinct chunk; the list is display.
        assert_eq!(facts[0].support_count, 5);

        // `as_of` keeps the latest value across merges (§8 Stage 4).
        let early = serde_json::json!({"as_of": "2020-01-01"});
        let late = serde_json::json!({"as_of": "2021-06-15"});
        store
            .merge_fact(
                "s",
                Predicate::AssociatedWith,
                "o",
                "ck9",
                Some(&early),
                caps,
            )
            .await
            .unwrap();
        store
            .merge_fact(
                "s",
                Predicate::AssociatedWith,
                "o",
                "ck10",
                Some(&late),
                caps,
            )
            .await
            .unwrap();
        let facts = store.facts_within_hops(&["s"], 1).await.unwrap();
        assert_eq!(
            facts[0]
                .properties
                .as_ref()
                .and_then(|p| p.get("as_of"))
                .and_then(serde_json::Value::as_str),
            Some("2021-06-15"),
            "as_of keeps the latest"
        );
    }

    #[tokio::test]
    async fn merge_fact_requires_existing_endpoints() {
        let store = LadybugStore::in_memory(4).unwrap();
        store
            .upsert_entity(&entity("s", "X", EntityType::Concept))
            .await
            .unwrap();
        let caps = crate::knowledge::FactCaps {
            max_evidence: 8,
            max_occurrences: 8,
        };
        let err = store
            .merge_fact("s", Predicate::PartOf, "ghost", "ck", None, caps)
            .await
            .unwrap_err();
        assert!(
            matches!(err, KnowledgeError::Backend(ref m) if m.contains("ghost")),
            "got {err:?}"
        );
    }
}
