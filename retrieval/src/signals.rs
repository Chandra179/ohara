//! Retrieval signals and score fusion.
//!
//! This module owns read-side ranking signals that are independent of the
//! vector provider: a bounded full-text scan over indexed artifacts and a
//! bounded chunk-to-entity path scan in `FalkorDB`. It does not change any
//! ingestion artifact or write to either derived store.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use redis::Value;
use serde::Deserialize;

use crate::{QdrantPoint, QueryChunk, point_to_chunk};

const GRAPH_QUERY_TIMEOUT: Duration = Duration::from_secs(1);
const GRAPH_ROW_LIMIT: usize = 10_000;
const VECTOR_WEIGHT: f32 = 0.70;
const FULL_TEXT_WEIGHT: f32 = 0.20;
const GRAPH_PATH_WEIGHT: f32 = 0.10;

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Availability {
    pub(crate) qdrant: bool,
    pub(crate) full_text: bool,
    pub(crate) graph_path: bool,
}

#[derive(Debug)]
pub(crate) struct ResultSet {
    pub(crate) chunks: Vec<QueryChunk>,
    pub(crate) availability: Availability,
}

#[derive(Debug, Deserialize)]
struct IndexedArtifact {
    chunks: Vec<IndexedChunk>,
}

#[derive(Debug, Clone, Deserialize)]
struct IndexedChunk {
    chunk_id: String,
    title: String,
    text: String,
}

#[derive(Debug, Clone)]
struct GraphPath {
    chunk_id: String,
    entity_name: String,
    length: usize,
}

#[derive(Debug)]
struct Candidate {
    chunk: QueryChunk,
    vector_score: f32,
    full_text_score: f32,
    graph_path_score: f32,
}

/// Combines vector, full-text, and graph-path evidence into one ranked result.
pub(crate) async fn combine(
    data_dir: &Path,
    falkordb_url: &str,
    graph_name: &str,
    points: &[QdrantPoint],
    question: &str,
    limit: usize,
) -> Result<ResultSet, std::io::Error> {
    let indexed = read_indexed_chunks(data_dir).await?;
    let full_text_matches = rank_full_text(question, &indexed, limit);
    let graph_paths = graph_paths(falkordb_url, graph_name).await;
    let graph_scores = graph_paths
        .as_deref()
        .map(|paths| score_graph_paths(question, paths));

    let mut candidates = BTreeMap::<String, Candidate>::new();
    for point in points {
        let Some(chunk) = point_to_chunk(point) else {
            continue;
        };
        let chunk_id = chunk.chunk_id.clone();
        candidates
            .entry(chunk_id)
            .and_modify(|candidate| {
                candidate.vector_score = candidate
                    .vector_score
                    .max(normalize_vector_score(point.score));
            })
            .or_insert_with(|| Candidate {
                chunk,
                vector_score: normalize_vector_score(point.score),
                full_text_score: 0.0,
                graph_path_score: 0.0,
            });
    }

    for (chunk_id, score) in full_text_matches {
        let Some(indexed_chunk) = indexed.get(&chunk_id) else {
            continue;
        };
        candidates
            .entry(chunk_id.clone())
            .and_modify(|candidate| candidate.full_text_score = score)
            .or_insert_with(|| Candidate {
                chunk: query_chunk(indexed_chunk),
                vector_score: 0.0,
                full_text_score: score,
                graph_path_score: 0.0,
            });
    }

    if let Some(scores) = graph_scores {
        for (chunk_id, score) in scores {
            let Some(indexed_chunk) = indexed.get(&chunk_id) else {
                if let Some(candidate) = candidates.get_mut(&chunk_id) {
                    candidate.graph_path_score = score;
                }
                continue;
            };
            candidates
                .entry(chunk_id.clone())
                .and_modify(|candidate| candidate.graph_path_score = score)
                .or_insert_with(|| Candidate {
                    chunk: query_chunk(indexed_chunk),
                    vector_score: 0.0,
                    full_text_score: 0.0,
                    graph_path_score: score,
                });
        }
    }

    let mut chunks: Vec<QueryChunk> = candidates
        .into_values()
        .map(|mut candidate| {
            candidate.chunk.score = fuse_scores(
                candidate.vector_score,
                candidate.full_text_score,
                candidate.graph_path_score,
            );
            candidate.chunk
        })
        .collect();
    chunks.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    chunks.truncate(limit);

    Ok(ResultSet {
        chunks,
        availability: Availability {
            qdrant: true,
            full_text: !indexed.is_empty(),
            graph_path: graph_paths.is_some(),
        },
    })
}

fn query_chunk(chunk: &IndexedChunk) -> QueryChunk {
    QueryChunk {
        chunk_id: chunk.chunk_id.clone(),
        score: 0.0,
        text: chunk.text.clone(),
    }
}

async fn read_indexed_chunks(
    data_dir: &Path,
) -> Result<BTreeMap<String, IndexedChunk>, std::io::Error> {
    let mut entries = tokio::fs::read_dir(data_dir.join("indexed")).await?;
    let mut chunks = BTreeMap::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file()
            || entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let path = entry.path();
        let bytes = tokio::fs::read(&path).await?;
        let artifact = match serde_json::from_slice::<IndexedArtifact>(&bytes) {
            Ok(artifact) => artifact,
            Err(error) => {
                eprintln!(
                    "ohara-retrieval: skipping invalid indexed artifact {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        for chunk in artifact.chunks {
            chunks.insert(chunk.chunk_id.clone(), chunk);
        }
    }
    Ok(chunks)
}

fn rank_full_text(
    question: &str,
    indexed: &BTreeMap<String, IndexedChunk>,
    limit: usize,
) -> Vec<(String, f32)> {
    let mut matches: Vec<(String, f32)> = indexed
        .values()
        .filter_map(|chunk| {
            let score = full_text_score(question, chunk);
            (score > 0.0).then(|| (chunk.chunk_id.clone(), score))
        })
        .collect();
    matches.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    matches.truncate(limit);
    matches
}

fn full_text_score(question: &str, chunk: &IndexedChunk) -> f32 {
    let query_terms = unique_terms(question);
    if query_terms.is_empty() {
        return 0.0;
    }
    let body_terms = unique_terms(&chunk.text);
    let title_terms = unique_terms(&chunk.title);
    let body_matches = query_terms
        .iter()
        .filter(|term| body_terms.contains(*term))
        .count();
    let title_matches = query_terms
        .iter()
        .filter(|term| title_terms.contains(*term))
        .count();
    let denominator = bounded_f32(query_terms.len());
    ((bounded_f32(body_matches) / denominator) * 0.8
        + (bounded_f32(title_matches) / denominator) * 0.2)
        .min(1.0)
}

fn unique_terms(value: &str) -> HashSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

async fn graph_paths(falkordb_url: &str, graph_name: &str) -> Option<Vec<GraphPath>> {
    let request = async {
        let redis_client = redis::Client::open(falkordb_url).map_err(|error| error.to_string())?;
        let mut connection = redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|error| error.to_string())?;
        let response: Value = redis::cmd("GRAPH.QUERY")
            .arg(graph_name)
            .arg(format!(
                "MATCH p=(c:Chunk)-[:MENTIONS]->(e:Entity) RETURN c.id, e.name, length(p) LIMIT {GRAPH_ROW_LIMIT}"
            ))
            .query_async(&mut connection)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(parse_graph_paths(&response))
    };
    match tokio::time::timeout(GRAPH_QUERY_TIMEOUT, request).await {
        Ok(Ok(paths)) => Some(paths),
        Ok(Err(error)) => {
            eprintln!("ohara-retrieval: graph signal unavailable: {error}");
            None
        }
        Err(_) => {
            eprintln!("ohara-retrieval: graph signal timed out");
            None
        }
    }
}

fn parse_graph_paths(value: &Value) -> Vec<GraphPath> {
    let Some(rows) = find_rows(value) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let Value::Array(columns) = row else {
                return None;
            };
            let chunk_id = value_string(columns.first()?)?;
            let entity_name = value_string(columns.get(1)?)?;
            let length = columns.get(2).and_then(value_usize).unwrap_or(1);
            Some(GraphPath {
                chunk_id,
                entity_name,
                length,
            })
        })
        .collect()
}

fn find_rows(value: &Value) -> Option<&[Value]> {
    let Value::Array(items) = value else {
        return None;
    };
    if !items.is_empty() && items.iter().all(|item| matches!(item, Value::Array(_))) {
        return Some(items);
    }
    items.iter().find_map(find_rows)
}

fn value_string(value: &Value) -> Option<String> {
    match value {
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::SimpleString(text) | Value::VerbatimString { text, .. } => Some(text.clone()),
        Value::Int(number) => Some(number.to_string()),
        _ => None,
    }
}

fn value_usize(value: &Value) -> Option<usize> {
    value_string(value)?.parse().ok()
}

fn score_graph_paths(question: &str, paths: &[GraphPath]) -> HashMap<String, f32> {
    let query_terms = unique_terms(question);
    if query_terms.is_empty() {
        return HashMap::new();
    }
    let denominator = bounded_f32(query_terms.len());
    let mut scores = HashMap::new();
    for path in paths {
        let entity_terms = unique_terms(&path.entity_name);
        let matched = query_terms
            .iter()
            .filter(|term| entity_terms.contains(*term))
            .count();
        if matched == 0 {
            continue;
        }
        let signal = (bounded_f32(matched) / denominator) / bounded_f32(path.length.max(1));
        scores
            .entry(path.chunk_id.clone())
            .and_modify(|score: &mut f32| *score = (*score + signal).min(1.0))
            .or_insert(signal.min(1.0));
    }
    scores
}

fn normalize_vector_score(score: f32) -> f32 {
    f32::midpoint(score, 1.0).clamp(0.0, 1.0)
}

fn bounded_f32(value: usize) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

fn fuse_scores(vector: f32, full_text: f32, graph_path: f32) -> f32 {
    (vector * VECTOR_WEIGHT + full_text * FULL_TEXT_WEIGHT + graph_path * GRAPH_PATH_WEIGHT)
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{GraphPath, IndexedChunk, full_text_score, parse_graph_paths, score_graph_paths};

    #[test]
    fn full_text_score_prefers_body_and_title_matches() {
        let chunk = IndexedChunk {
            chunk_id: "chunk-1".into(),
            title: "Rust retrieval".into(),
            text: "Rust retrieval combines local evidence.".into(),
        };
        assert!((full_text_score("Rust retrieval", &chunk) - 1.0).abs() < f32::EPSILON);
        assert!(full_text_score("Python", &chunk).abs() < f32::EPSILON);
    }

    #[test]
    fn graph_signal_requires_a_query_entity_match() {
        let paths = vec![GraphPath {
            chunk_id: "chunk-1".into(),
            entity_name: "Rust Foundation".into(),
            length: 1,
        }];
        assert!(score_graph_paths("Tell me about Rust", &paths).contains_key("chunk-1"));
        assert!(score_graph_paths("Tell me about Python", &paths).is_empty());
    }

    #[test]
    fn parses_graph_query_rows() {
        let response = redis::Value::Array(vec![
            redis::Value::Array(vec![
                redis::Value::BulkString(b"c.id".to_vec()),
                redis::Value::BulkString(b"e.name".to_vec()),
                redis::Value::BulkString(b"length(p)".to_vec()),
            ]),
            redis::Value::Array(vec![redis::Value::Array(vec![
                redis::Value::BulkString(b"chunk-1".to_vec()),
                redis::Value::BulkString(b"Rust".to_vec()),
                redis::Value::Int(1),
            ])]),
            redis::Value::SimpleString("Cached execution: 0".into()),
        ]);
        let paths = parse_graph_paths(&response);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].chunk_id, "chunk-1");
        assert_eq!(paths[0].entity_name, "Rust");
        assert_eq!(paths[0].length, 1);
    }
}
