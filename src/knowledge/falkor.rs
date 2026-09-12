//! `FalkorDB` graph adapter.
//!
//! `FalkorDB` is the sole owner of graph state. The adapter sends `OpenCypher`
//! through `FalkorDB`'s Redis command interface and keeps all query text and
//! response decoding local to this module.

use std::collections::HashSet;
use std::time::Duration;

use redis::Value;
use serde_json::{Map, Value as JsonValue};
use url::Url;

use crate::knowledge::{EntityRecord, Fact, FactCaps, KnowledgeError, Predicate};

const EVIDENCE_KEY: &str = "evidence";
const OCCURRENCES_KEY: &str = "occurrences";

#[derive(Clone)]
pub(super) struct FalkorGraph {
    client: redis::Client,
    graph_name: String,
    timeout: Duration,
    read_only: bool,
}

pub(super) enum EntityDeleteOutcome {
    Deleted,
    Referenced,
    Absent,
}

#[derive(Debug, Clone)]
struct GraphFact {
    subject_id: String,
    predicate: Predicate,
    object_id: String,
    support: u64,
    properties: Option<String>,
}

impl FalkorGraph {
    pub(super) fn new(
        url: &Url,
        graph_name: &str,
        timeout: Duration,
        read_only: bool,
    ) -> Result<Self, KnowledgeError> {
        let client = redis::Client::open(url.as_str())
            .map_err(|error| KnowledgeError::Backend(format!("create FalkorDB client: {error}")))?;
        Ok(Self {
            client,
            graph_name: graph_name.to_string(),
            timeout,
            read_only,
        })
    }

    pub(super) async fn health(&self) -> Result<(), KnowledgeError> {
        let mut connection = self.connection().await?;
        let result: String = tokio::time::timeout(
            self.timeout,
            redis::cmd("PING").query_async(&mut connection),
        )
        .await
        .map_err(|_| unavailable("FalkorDB health check timed out"))?
        .map_err(|error| unavailable(format!("FalkorDB health check: {error}")))?;
        if result.eq_ignore_ascii_case("PONG") {
            Ok(())
        } else {
            Err(KnowledgeError::Backend(format!(
                "FalkorDB health check returned {result:?}"
            )))
        }
    }

    pub(super) async fn ensure_chunk(
        &self,
        chunk_id: &str,
        doc_id: &str,
    ) -> Result<(), KnowledgeError> {
        self.write(&format!(
            "MERGE (c:Chunk {{chunk_id: {}}}) SET c.doc_id = {}",
            cypher_string(chunk_id),
            cypher_string(doc_id)
        ))
        .await
    }

    pub(super) async fn upsert_entity(&self, entity: &EntityRecord) -> Result<(), KnowledgeError> {
        self.write(&format!(
            "MERGE (e:Entity {{entity_id: {}}}) SET e.canonical_name = {}, e.entity_type = {}, e.subtype = {}",
            cypher_string(&entity.entity_id),
            cypher_string(&entity.canonical_name),
            cypher_string(entity.entity_type.as_str()),
            cypher_string(entity.subtype.as_deref().unwrap_or_default())
        ))
        .await
    }

    pub(super) async fn link_mention(
        &self,
        chunk_id: &str,
        entity_id: &str,
    ) -> Result<(), KnowledgeError> {
        self.write(&format!(
            "MERGE (c:Chunk {{chunk_id: {}}}) MERGE (e:Entity {{entity_id: {}}}) MERGE (c)-[m:MENTIONS]->(e) ON CREATE SET m.support = 1",
            cypher_string(chunk_id),
            cypher_string(entity_id)
        ))
        .await
    }

    pub(super) async fn fold_entity(
        &self,
        loser: &str,
        winner: &str,
    ) -> Result<(), KnowledgeError> {
        if loser == winner {
            return Ok(());
        }
        let mention_rows = self
            .rows(&format!(
                "MATCH (c:Chunk)-[m:MENTIONS]->(e:Entity {{entity_id: {}}}) RETURN c.chunk_id, m.support",
                cypher_string(loser)
            ))
            .await?;
        for row in mention_rows {
            let chunk_id = row_string(&row, 0)?;
            let support = row_u64(&row, 1)?.max(1);
            self.merge_mention(&chunk_id, winner, support).await?;
        }

        let fact_rows = self
            .rows(&format!(
                "MATCH (s:Entity)-[f:FACT]->(o:Entity) WHERE s.entity_id = {} OR o.entity_id = {} RETURN s.entity_id, f.predicate, o.entity_id, f.support, f.properties",
                cypher_string(loser),
                cypher_string(loser)
            ))
            .await?;
        for row in fact_rows {
            let mut fact = graph_fact(&row)?;
            if fact.subject_id == loser {
                fact.subject_id = winner.to_string();
            }
            if fact.object_id == loser {
                fact.object_id = winner.to_string();
            }
            self.merge_fact_state(&fact).await?;
        }

        self.write(&format!(
            "MATCH (e:Entity {{entity_id: {}}}) DETACH DELETE e",
            cypher_string(loser)
        ))
        .await
    }

    pub(super) async fn merge_fact(
        &self,
        subject_id: &str,
        predicate: Predicate,
        object_id: &str,
        evidence_chunk: &str,
        properties: Option<&JsonValue>,
        caps: FactCaps,
    ) -> Result<(), KnowledgeError> {
        let endpoint_rows = self
            .rows(&format!(
                "MATCH (e:Entity) WHERE e.entity_id IN [{}, {}] RETURN e.entity_id",
                cypher_string(subject_id),
                cypher_string(object_id)
            ))
            .await?;
        let endpoint_count = endpoint_rows
            .iter()
            .map(|row| row_string(row, 0))
            .collect::<Result<HashSet<_>, _>>()?
            .len();
        if endpoint_count != 2 {
            return Err(KnowledgeError::Backend(format!(
                "fact endpoints are missing: {subject_id} -> {object_id}"
            )));
        }

        let existing = self
            .rows(&format!(
                "MATCH (s:Entity {{entity_id: {}}})-[f:FACT {{predicate: {}}}]->(o:Entity {{entity_id: {}}}) RETURN s.entity_id, f.predicate, o.entity_id, f.support, f.properties",
                cypher_string(subject_id),
                cypher_string(predicate.as_str()),
                cypher_string(object_id)
            ))
            .await?;
        let current = existing.first().map(|row| graph_fact(row)).transpose()?;
        let (support, merged_properties) = merge_new_assertion(
            current.as_ref().and_then(|fact| fact.properties.as_deref()),
            current.as_ref().map_or(0, |fact| fact.support),
            evidence_chunk,
            properties,
            caps,
        );
        self.write(&format!(
            "MATCH (s:Entity {{entity_id: {}}}), (o:Entity {{entity_id: {}}}) MERGE (s)-[f:FACT {{predicate: {}}}]->(o) SET f.support = {}, f.properties = {}",
            cypher_string(subject_id),
            cypher_string(object_id),
            cypher_string(predicate.as_str()),
            support,
            cypher_string(&merged_properties)
        ))
        .await
    }

    pub(super) async fn delete_document(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        self.write(&format!(
            "MATCH (c:Chunk {{doc_id: {}}}) DETACH DELETE c",
            cypher_string(doc_id)
        ))
        .await
    }

    pub(super) async fn delete_entity(
        &self,
        entity_id: &str,
    ) -> Result<EntityDeleteOutcome, KnowledgeError> {
        let rows = self
            .rows(&format!(
                "MATCH (e:Entity {{entity_id: {}}}) OPTIONAL MATCH (e)-[r]-() RETURN count(DISTINCT e), count(r)",
                cypher_string(entity_id)
            ))
            .await?;
        let Some(row) = rows.first() else {
            return Ok(EntityDeleteOutcome::Absent);
        };
        let count = row_u64(row, 0)?;
        if count == 0 {
            return Ok(EntityDeleteOutcome::Absent);
        }
        if row_u64(row, 1)? > 0 {
            return Ok(EntityDeleteOutcome::Referenced);
        }
        self.write(&format!(
            "MATCH (e:Entity {{entity_id: {}}}) DELETE e",
            cypher_string(entity_id)
        ))
        .await?;
        Ok(EntityDeleteOutcome::Deleted)
    }

    pub(super) async fn chunks_for_entities(
        &self,
        ids: &[&str],
    ) -> Result<Vec<String>, KnowledgeError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids
            .iter()
            .map(|id| cypher_string(id))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = self
            .rows(&format!(
                "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) WHERE e.entity_id IN [{list}] RETURN DISTINCT c.chunk_id ORDER BY c.chunk_id"
            ))
            .await?;
        rows.iter().map(|row| row_string(row, 0)).collect()
    }

    pub(super) async fn facts_within_hops(
        &self,
        ids: &[&str],
        hops: u8,
    ) -> Result<Vec<Fact>, KnowledgeError> {
        if ids.is_empty() || hops == 0 {
            return Ok(Vec::new());
        }
        let rows = self
            .rows("MATCH (s:Entity)-[f:FACT]->(o:Entity) RETURN s.entity_id, f.predicate, o.entity_id, f.support, f.properties")
            .await?;
        let facts: Vec<GraphFact> = rows
            .iter()
            .map(|row| graph_fact(row))
            .collect::<Result<_, _>>()?;
        let mut frontier: HashSet<String> = ids.iter().map(|id| (*id).to_string()).collect();
        let mut seen = HashSet::new();
        let mut output = Vec::new();
        for _ in 0..hops {
            let mut next = HashSet::new();
            for fact in &facts {
                if frontier.contains(&fact.subject_id) || frontier.contains(&fact.object_id) {
                    let key = (
                        fact.subject_id.clone(),
                        fact.predicate,
                        fact.object_id.clone(),
                    );
                    if seen.insert(key) {
                        output.push(Fact {
                            subject_id: fact.subject_id.clone(),
                            predicate: fact.predicate,
                            object_id: fact.object_id.clone(),
                            support_count: fact.support,
                            properties: fact
                                .properties
                                .as_deref()
                                .and_then(|raw| serde_json::from_str(raw).ok()),
                        });
                    }
                    next.insert(fact.subject_id.clone());
                    next.insert(fact.object_id.clone());
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(output)
    }

    async fn merge_fact_state(&self, fact: &GraphFact) -> Result<(), KnowledgeError> {
        let current_rows = self
            .rows(&format!(
                "MATCH (s:Entity {{entity_id: {}}})-[f:FACT {{predicate: {}}}]->(o:Entity {{entity_id: {}}}) RETURN f.support, f.properties",
                cypher_string(&fact.subject_id),
                cypher_string(fact.predicate.as_str()),
                cypher_string(&fact.object_id)
            ))
            .await?;
        let (support, properties) = if let Some(row) = current_rows.first() {
            (
                row_u64(row, 0)?.saturating_add(fact.support),
                merge_edge_properties(
                    row_string_optional(row, 1)?.as_deref(),
                    fact.properties.as_deref(),
                ),
            )
        } else {
            (
                fact.support,
                merge_edge_properties(None, fact.properties.as_deref()),
            )
        };
        self.write(&format!(
            "MATCH (s:Entity {{entity_id: {}}}), (o:Entity {{entity_id: {}}}) MERGE (s)-[f:FACT {{predicate: {}}}]->(o) SET f.support = {}, f.properties = {}",
            cypher_string(&fact.subject_id),
            cypher_string(&fact.object_id),
            cypher_string(fact.predicate.as_str()),
            support,
            cypher_string(&properties)
        ))
        .await
    }

    async fn merge_mention(
        &self,
        chunk_id: &str,
        entity_id: &str,
        support: u64,
    ) -> Result<(), KnowledgeError> {
        self.write(&format!(
            "MERGE (c:Chunk {{chunk_id: {}}}) MERGE (e:Entity {{entity_id: {}}}) MERGE (c)-[m:MENTIONS]->(e) ON CREATE SET m.support = {} ON MATCH SET m.support = coalesce(m.support, 0) + {}",
            cypher_string(chunk_id),
            cypher_string(entity_id),
            support,
            support
        ))
        .await
    }

    async fn write(&self, query: &str) -> Result<(), KnowledgeError> {
        self.require_writable()?;
        self.run(query).await.map(|_| ())
    }

    async fn rows(&self, query: &str) -> Result<Vec<Vec<Value>>, KnowledgeError> {
        self.run(query).await
    }

    async fn run(&self, query: &str) -> Result<Vec<Vec<Value>>, KnowledgeError> {
        let mut connection = self.connection().await?;
        let result: Value = tokio::time::timeout(
            self.timeout,
            redis::cmd("GRAPH.QUERY")
                .arg(&self.graph_name)
                .arg(query)
                .query_async(&mut connection),
        )
        .await
        .map_err(|_| unavailable("FalkorDB query timed out"))?
        .map_err(|error| map_redis_error("FalkorDB query", &error))?;
        parse_rows(result)
    }

    async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, KnowledgeError> {
        tokio::time::timeout(self.timeout, self.client.get_multiplexed_async_connection())
            .await
            .map_err(|_| unavailable("FalkorDB connection timed out"))?
            .map_err(|error| unavailable(format!("FalkorDB connection: {error}")))
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

fn parse_rows(value: Value) -> Result<Vec<Vec<Value>>, KnowledgeError> {
    let Value::Array(mut parts) = value else {
        return Err(KnowledgeError::Backend(
            "FalkorDB returned a non-array graph response".into(),
        ));
    };
    if parts.len() < 2 {
        return Ok(Vec::new());
    }
    let rows = parts.remove(1);
    match rows {
        Value::Array(rows) => rows
            .into_iter()
            .map(|row| match row {
                Value::Array(values) => Ok(values),
                other => Err(KnowledgeError::Backend(format!(
                    "FalkorDB row is not an array: {other:?}"
                ))),
            })
            .collect(),
        other => Err(KnowledgeError::Backend(format!(
            "FalkorDB result rows are not an array: {other:?}"
        ))),
    }
}

fn graph_fact(row: &[Value]) -> Result<GraphFact, KnowledgeError> {
    Ok(GraphFact {
        subject_id: row_string(row, 0)?,
        predicate: row_string(row, 1)?.parse()?,
        object_id: row_string(row, 2)?,
        support: row_u64(row, 3)?,
        properties: row_string_optional(row, 4)?,
    })
}

fn row_value(row: &[Value], index: usize) -> Result<&Value, KnowledgeError> {
    row.get(index).ok_or_else(|| {
        KnowledgeError::Backend(format!("FalkorDB result row is missing column {index}"))
    })
}

fn row_string(row: &[Value], index: usize) -> Result<String, KnowledgeError> {
    match row_value(row, index)? {
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).map_err(|error| {
            KnowledgeError::Backend(format!("FalkorDB string is not UTF-8: {error}"))
        }),
        Value::SimpleString(value) => Ok(value.clone()),
        Value::Int(value) => Ok(value.to_string()),
        other => Err(KnowledgeError::Backend(format!(
            "FalkorDB column {index} is not a scalar string: {other:?}"
        ))),
    }
}

fn row_string_optional(row: &[Value], index: usize) -> Result<Option<String>, KnowledgeError> {
    match row_value(row, index)? {
        Value::Nil => Ok(None),
        _ => row_string(row, index).map(Some),
    }
}

fn row_u64(row: &[Value], index: usize) -> Result<u64, KnowledgeError> {
    match row_value(row, index)? {
        Value::Int(value) if *value >= 0 => u64::try_from(*value).map_err(|error| {
            KnowledgeError::Backend(format!("FalkorDB integer is out of range: {error}"))
        }),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).parse().map_err(|error| {
            KnowledgeError::Backend(format!(
                "FalkorDB value is not an unsigned integer: {error}"
            ))
        }),
        Value::SimpleString(value) => value.parse().map_err(|error| {
            KnowledgeError::Backend(format!(
                "FalkorDB value is not an unsigned integer: {error}"
            ))
        }),
        other => Err(KnowledgeError::Backend(format!(
            "FalkorDB column {index} is not an unsigned integer: {other:?}"
        ))),
    }
}

fn merge_new_assertion(
    existing_properties: Option<&str>,
    existing_support: u64,
    evidence_chunk: &str,
    properties: Option<&JsonValue>,
    caps: FactCaps,
) -> (u64, String) {
    let (mut evidence, mut occurrences, mut caller_properties) =
        edge_state(existing_properties.unwrap_or(""));
    let is_new = !evidence.iter().any(|item| item == evidence_chunk);
    if is_new && evidence.len() < caps.max_evidence {
        evidence.push(evidence_chunk.to_string());
    }
    if let Some(properties) = properties {
        merge_occurrences(&mut occurrences, properties, caps.max_occurrences);
        if let JsonValue::Object(incoming) = properties
            && let JsonValue::Object(current) = &mut caller_properties
        {
            for (key, value) in incoming {
                if key != EVIDENCE_KEY && key != OCCURRENCES_KEY && key != "occurred_on" {
                    current.insert(key.clone(), value.clone());
                }
            }
        }
    }
    if let JsonValue::Object(map) = &mut caller_properties {
        map.insert(
            EVIDENCE_KEY.into(),
            JsonValue::Array(evidence.into_iter().map(JsonValue::String).collect()),
        );
        map.insert(
            OCCURRENCES_KEY.into(),
            JsonValue::Array(occurrences.into_iter().map(JsonValue::String).collect()),
        );
    }
    (
        existing_support.saturating_add(u64::from(is_new)),
        serde_json::to_string(&caller_properties).unwrap_or_else(|_| "{}".into()),
    )
}

fn edge_state(raw: &str) -> (Vec<String>, Vec<String>, JsonValue) {
    let mut parsed: JsonValue =
        serde_json::from_str(raw).unwrap_or_else(|_| JsonValue::Object(Map::new()));
    let evidence = string_list(parsed.get(EVIDENCE_KEY));
    let occurrences = string_list(parsed.get(OCCURRENCES_KEY));
    if let JsonValue::Object(map) = &mut parsed {
        map.remove(EVIDENCE_KEY);
        map.remove(OCCURRENCES_KEY);
        map.remove("occurred_on");
    }
    (evidence, occurrences, parsed)
}

fn merge_occurrences(values: &mut Vec<String>, properties: &JsonValue, cap: usize) {
    let Some(value) = properties.get("occurred_on") else {
        return;
    };
    let incoming = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
        .chain(value.as_str())
        .map(str::to_owned);
    for item in incoming {
        if values.len() >= cap {
            break;
        }
        if !values.iter().any(|existing| existing == &item) {
            values.push(item);
        }
    }
}

fn string_list(value: Option<&JsonValue>) -> Vec<String> {
    value
        .and_then(JsonValue::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn merge_edge_properties(existing: Option<&str>, incoming: Option<&str>) -> String {
    let (mut evidence, mut occurrences, mut properties) = edge_state(existing.unwrap_or(""));
    let (new_evidence, new_occurrences, new_properties) = edge_state(incoming.unwrap_or(""));
    for item in new_evidence {
        if !evidence.iter().any(|existing| existing == &item) {
            evidence.push(item);
        }
    }
    for item in new_occurrences {
        if !occurrences.iter().any(|existing| existing == &item) {
            occurrences.push(item);
        }
    }
    if let (JsonValue::Object(current), JsonValue::Object(incoming)) =
        (&mut properties, new_properties)
    {
        current.extend(incoming);
    }
    if let JsonValue::Object(map) = &mut properties {
        map.insert(
            EVIDENCE_KEY.into(),
            JsonValue::Array(evidence.into_iter().map(JsonValue::String).collect()),
        );
        map.insert(
            OCCURRENCES_KEY.into(),
            JsonValue::Array(occurrences.into_iter().map(JsonValue::String).collect()),
        );
    }
    serde_json::to_string(&properties).unwrap_or_else(|_| "{}".into())
}

fn cypher_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('\'');
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            _ => escaped.push(character),
        }
    }
    escaped.push('\'');
    escaped
}

fn unavailable(message: impl Into<String>) -> KnowledgeError {
    KnowledgeError::Unavailable(message.into())
}

fn map_redis_error(context: &str, error: &redis::RedisError) -> KnowledgeError {
    if matches!(
        error.kind(),
        redis::ErrorKind::IoError | redis::ErrorKind::ClusterConnectionNotFound
    ) {
        unavailable(format!("{context}: {error}"))
    } else {
        KnowledgeError::Backend(format!("{context}: {error}"))
    }
}
