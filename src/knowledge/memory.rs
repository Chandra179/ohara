//! In-memory [`KnowledgeStore`](super::KnowledgeStore) used by contract tests.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::{
    ChunkFilter, EntityRecord, Fact, FactCaps, KnowledgeError, KnowledgeStore, Predicate,
    ScoredHit, VectorSpace,
};

#[derive(Clone)]
struct VectorRecord {
    doc_id: String,
    vector: Vec<f32>,
}

#[derive(Clone)]
struct FactRecord {
    subject_id: String,
    predicate: Predicate,
    object_id: String,
    support: u64,
    evidence: Vec<String>,
    occurrences: Vec<String>,
}

/// Small deterministic store for tests and injected local adapters.
pub struct InMemoryKnowledge {
    vectors: Mutex<HashMap<(String, String), VectorRecord>>,
    entities: Mutex<HashMap<String, EntityRecord>>,
    mentions: Mutex<HashSet<(String, String)>>,
    facts: Mutex<HashMap<(String, Predicate, String), FactRecord>>,
    graph_traversal: bool,
}

impl Default for InMemoryKnowledge {
    fn default() -> Self {
        Self {
            vectors: Mutex::new(HashMap::new()),
            entities: Mutex::new(HashMap::new()),
            mentions: Mutex::new(HashSet::new()),
            facts: Mutex::new(HashMap::new()),
            graph_traversal: true,
        }
    }
}

impl InMemoryKnowledge {
    /// Creates a store with graph traversal disabled for degradation tests.
    #[must_use]
    pub fn without_graph() -> Self {
        Self {
            graph_traversal: false,
            ..Self::default()
        }
    }

    fn vector_key(space: &VectorSpace, id: &str) -> (String, String) {
        (format!("{space:?}"), id.to_string())
    }

    fn cosine(left: &[f32], right: &[f32]) -> f32 {
        let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
        let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
        let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
        if left_norm == 0.0 || right_norm == 0.0 {
            0.0
        } else {
            dot / (left_norm * right_norm)
        }
    }

    #[cfg(test)]
    pub(crate) fn remove(&self, id: &str) {
        if let Ok(mut vectors) = self.vectors.lock() {
            vectors.retain(|(_, vector_id), _| vector_id != id);
        }
    }

    #[cfg(test)]
    pub(crate) fn facts(&self) -> Vec<Fact> {
        self.facts
            .lock()
            .map(|facts| {
                facts
                    .values()
                    .map(|fact| Fact {
                        subject_id: fact.subject_id.clone(),
                        predicate: fact.predicate,
                        object_id: fact.object_id.clone(),
                        support_count: fact.support,
                        properties: Some(serde_json::json!({
                            "evidence": fact.evidence,
                            "occurrences": fact.occurrences,
                        })),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl KnowledgeStore for InMemoryKnowledge {
    fn capabilities(&self) -> super::KsCapabilities {
        super::KsCapabilities {
            filtered_ann: false,
            graph_traversal: self.graph_traversal,
        }
    }

    async fn upsert_vectors(
        &self,
        space: VectorSpace,
        doc_id: &str,
        ids: &[&str],
        vectors: &[Vec<f32>],
    ) -> Result<(), KnowledgeError> {
        if ids.len() != vectors.len() {
            return Err(KnowledgeError::Backend(
                "vector id/value length mismatch".into(),
            ));
        }
        let mut store = self
            .vectors
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory vector lock poisoned".into()))?;
        for (id, vector) in ids.iter().zip(vectors) {
            store.insert(
                Self::vector_key(&space, id),
                VectorRecord {
                    doc_id: doc_id.to_string(),
                    vector: vector.clone(),
                },
            );
        }
        Ok(())
    }

    async fn knn(
        &self,
        space: VectorSpace,
        query: &[f32],
        limit: usize,
        _filter: &ChunkFilter,
    ) -> Result<Vec<ScoredHit>, KnowledgeError> {
        let store = self
            .vectors
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory vector lock poisoned".into()))?;
        let mut hits: Vec<_> = store
            .iter()
            .filter(|((stored_space, _), _)| stored_space == &format!("{space:?}"))
            .map(|((_, id), record)| ScoredHit {
                id: id.clone(),
                score: Self::cosine(query, &record.vector),
            })
            .collect();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then(left.id.cmp(&right.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    async fn has_vector(&self, space: VectorSpace, id: &str) -> Result<bool, KnowledgeError> {
        Ok(self
            .vectors
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory vector lock poisoned".into()))?
            .contains_key(&Self::vector_key(&space, id)))
    }

    async fn upsert_entity(&self, entity: &EntityRecord) -> Result<(), KnowledgeError> {
        self.entities
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory entity lock poisoned".into()))?
            .insert(entity.entity_id.clone(), entity.clone());
        Ok(())
    }

    async fn link_mention(&self, chunk_id: &str, entity_id: &str) -> Result<(), KnowledgeError> {
        self.mentions
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory mention lock poisoned".into()))?
            .insert((chunk_id.to_string(), entity_id.to_string()));
        Ok(())
    }

    async fn fold_entity(&self, loser: &str, winner: &str) -> Result<(), KnowledgeError> {
        if loser == winner {
            return Ok(());
        }
        self.entities
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory entity lock poisoned".into()))?
            .remove(loser);
        let mut mentions = self
            .mentions
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory mention lock poisoned".into()))?;
        let moved: Vec<_> = mentions
            .iter()
            .filter(|(_, entity)| entity == loser)
            .map(|(chunk, _)| (chunk.clone(), winner.to_string()))
            .collect();
        mentions.retain(|(_, entity)| entity != loser);
        mentions.extend(moved);
        drop(mentions);

        let mut facts = self
            .facts
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory fact lock poisoned".into()))?;
        let existing = std::mem::take(&mut *facts);
        for (_, mut fact) in existing {
            if fact.subject_id == loser {
                fact.subject_id = winner.to_string();
            }
            if fact.object_id == loser {
                fact.object_id = winner.to_string();
            }
            let key = (
                fact.subject_id.clone(),
                fact.predicate,
                fact.object_id.clone(),
            );
            let entry = facts.entry(key).or_insert_with(|| FactRecord {
                subject_id: fact.subject_id.clone(),
                predicate: fact.predicate,
                object_id: fact.object_id.clone(),
                support: 0,
                evidence: Vec::new(),
                occurrences: Vec::new(),
            });
            entry.support = entry.support.saturating_add(fact.support);
            union_capped(&mut entry.evidence, fact.evidence, usize::MAX);
            union_capped(&mut entry.occurrences, fact.occurrences, usize::MAX);
        }
        Ok(())
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
        if !self
            .entities
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory entity lock poisoned".into()))?
            .contains_key(subject_id)
            || !self
                .entities
                .lock()
                .map_err(|_| KnowledgeError::Unavailable("memory entity lock poisoned".into()))?
                .contains_key(object_id)
        {
            return Err(KnowledgeError::Backend("fact endpoint is missing".into()));
        }
        let mut facts = self
            .facts
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory fact lock poisoned".into()))?;
        let entry = facts
            .entry((subject_id.to_string(), predicate, object_id.to_string()))
            .or_insert_with(|| FactRecord {
                subject_id: subject_id.to_string(),
                predicate,
                object_id: object_id.to_string(),
                support: 0,
                evidence: Vec::new(),
                occurrences: Vec::new(),
            });
        if !entry.evidence.iter().any(|item| item == evidence_chunk) {
            entry.support = entry.support.saturating_add(1);
            union_capped(
                &mut entry.evidence,
                vec![evidence_chunk.to_string()],
                caps.max_evidence,
            );
        }
        if let Some(value) = properties.and_then(|value| value.get("occurred_on")) {
            let values = value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .chain(value.as_str())
                .map(str::to_owned)
                .collect();
            union_capped(&mut entry.occurrences, values, caps.max_occurrences);
        }
        Ok(())
    }

    async fn delete_doc(&self, doc_id: &str) -> Result<(), KnowledgeError> {
        let mut deleted = HashSet::new();
        self.vectors
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory vector lock poisoned".into()))?
            .retain(|(space, id), record| {
                let keep = record.doc_id != doc_id;
                if !keep && space.starts_with("Chunks") {
                    deleted.insert(id.clone());
                }
                keep
            });
        self.mentions
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory mention lock poisoned".into()))?
            .retain(|(chunk, _)| !deleted.contains(chunk));
        Ok(())
    }

    async fn delete_entity(&self, entity_id: &str) -> Result<bool, KnowledgeError> {
        let exists = self
            .entities
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory entity lock poisoned".into()))?
            .contains_key(entity_id);
        if !exists {
            self.vectors
                .lock()
                .map_err(|_| KnowledgeError::Unavailable("memory vector lock poisoned".into()))?
                .remove(&Self::vector_key(&VectorSpace::EntityNames, entity_id));
            return Ok(false);
        }
        let mentioned = self
            .mentions
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory mention lock poisoned".into()))?
            .iter()
            .any(|(_, id)| id == entity_id);
        let referenced = self
            .facts
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory fact lock poisoned".into()))?
            .keys()
            .any(|(subject, _, object)| subject == entity_id || object == entity_id);
        if mentioned || referenced {
            return Ok(false);
        }
        self.entities
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory entity lock poisoned".into()))?
            .remove(entity_id);
        self.vectors
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory vector lock poisoned".into()))?
            .remove(&Self::vector_key(&VectorSpace::EntityNames, entity_id));
        Ok(true)
    }

    async fn chunks_for_entities(&self, ids: &[&str]) -> Result<Vec<String>, KnowledgeError> {
        let mentions = self
            .mentions
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory mention lock poisoned".into()))?;
        let mut chunks: Vec<_> = mentions
            .iter()
            .filter(|(_, entity)| ids.iter().any(|id| id == entity))
            .map(|(chunk, _)| chunk.clone())
            .collect();
        chunks.sort();
        chunks.dedup();
        Ok(chunks)
    }

    async fn facts_within_hops(&self, ids: &[&str], hops: u8) -> Result<Vec<Fact>, KnowledgeError> {
        if ids.is_empty() || hops == 0 {
            return Ok(Vec::new());
        }
        let facts = self
            .facts
            .lock()
            .map_err(|_| KnowledgeError::Unavailable("memory fact lock poisoned".into()))?;
        let mut frontier: HashSet<String> = ids.iter().map(|id| (*id).to_string()).collect();
        let mut seen = HashSet::new();
        let mut output = Vec::new();
        for _ in 0..hops {
            let mut next = HashSet::new();
            for fact in facts.values() {
                if frontier.contains(&fact.subject_id) || frontier.contains(&fact.object_id) {
                    let key = (
                        fact.subject_id.clone(),
                        fact.predicate,
                        fact.object_id.clone(),
                    );
                    if seen.insert(key) {
                        output.push(Fact { subject_id: fact.subject_id.clone(), predicate: fact.predicate, object_id: fact.object_id.clone(), support_count: fact.support, properties: Some(serde_json::json!({ "evidence": fact.evidence, "occurrences": fact.occurrences })) });
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
}

fn union_capped(target: &mut Vec<String>, incoming: Vec<String>, cap: usize) {
    for value in incoming {
        if target.len() >= cap {
            break;
        }
        if !target.iter().any(|existing| existing == &value) {
            target.push(value);
        }
    }
}
