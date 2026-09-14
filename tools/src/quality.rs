//! Golden retrieval-quality benchmark.

use crate::artifacts::{seed_directories, temporary_directory};
use crate::http::{client, free_port, request_json, wait_for_http};
use crate::metrics::{deterministic_embedding, mean_reciprocal_rank, ndcg_at_k, recall_at_k};
use crate::process::{ManagedProcess, environment};
use crate::providers::{start_ollama_server, start_quality_qdrant_server};
use crate::{Error, Result, check, repository_root};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const DATASET_PATH: &str = "docs/architecture/fixtures/retrieval-golden-v1.json";
const EMBEDDING_DIMENSION: usize = 384;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dataset {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    cutoffs: Vec<usize>,
    documents: Vec<Document>,
    queries: Vec<Query>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    #[serde(rename = "chunkId")]
    chunk_id: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct Query {
    id: String,
    #[serde(rename = "query")]
    text: String,
    relevance: BTreeMap<String, i64>,
}

/// Execute the deterministic golden retrieval benchmark.
///
/// # Errors
///
/// Returns an error when the golden dataset, retrieval process, or provider
/// response violates the retrieval contract.
pub async fn run() -> Result<()> {
    let root = repository_root()?;
    let dataset_path = root.join(DATASET_PATH);
    let dataset: Dataset = serde_json::from_slice(
        &std::fs::read(&dataset_path)
            .map_err(|source| Error::io("read retrieval golden dataset", source))?,
    )
    .map_err(|source| Error::Json {
        context: dataset_path.display().to_string(),
        source,
    })?;
    validate_dataset(&dataset)?;
    crate::metrics::metric_regression_tests()?;

    let mut qdrant = start_quality_qdrant_server().await?;
    let mut ollama = start_ollama_server().await?;
    qdrant
        .add_points(dataset.documents.iter().map(|document| {
            json!({
                "id": document.chunk_id,
                "vector": deterministic_embedding(&document.text),
                "payload": {"chunkId": document.chunk_id, "text": document.text},
            })
        }))
        .await;
    let temporary = temporary_directory("ohara-retrieval-quality")?;
    let data_dir = temporary.join("data");
    let log_dir = temporary.join("logs");
    seed_directories(&data_dir)?;
    std::fs::create_dir_all(&log_dir).map_err(|source| Error::io("create quality logs", source))?;
    let retrieval_port = free_port()?;
    let environment = environment([
        ("OHARA_DATA_DIR".to_owned(), data_dir.display().to_string()),
        (
            "OHARA_QDRANT_URL".to_owned(),
            format!("http://127.0.0.1:{}", qdrant.port()),
        ),
        (
            "OHARA_FALKORDB_URL".to_owned(),
            "redis://127.0.0.1:1".to_owned(),
        ),
        (
            "OHARA_LLM_URL".to_owned(),
            format!("http://127.0.0.1:{}", ollama.port()),
        ),
        ("OHARA_LLM_MODEL".to_owned(), "fixture".to_owned()),
        (
            "OHARA_EMBEDDING_MODE".to_owned(),
            "deterministic".to_owned(),
        ),
        (
            "OHARA_RETRIEVAL_BIND".to_owned(),
            format!("127.0.0.1:{retrieval_port}"),
        ),
    ]);
    let mut process = ManagedProcess::start(
        &root,
        "retrieval-quality",
        "ohara-retrieval",
        &environment,
        &log_dir,
    )?;
    let http_client = client()?;
    let result = run_queries(&http_client, retrieval_port, &dataset).await;
    let stop_result = process.stop().await;
    if result.is_err() {
        let output = process.log()?;
        if !output.is_empty() {
            eprintln!("--- retrieval-quality log ---\n{output}");
        }
    }
    qdrant.shutdown().await?;
    ollama.shutdown().await?;
    let _ = std::fs::remove_dir_all(&temporary);
    stop_result?;
    result
}

async fn run_queries(client: &reqwest::Client, port: u16, dataset: &Dataset) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}");
    wait_for_http(
        client,
        &format!("{url}/api/health"),
        None,
        Duration::from_secs(15),
    )
    .await?;
    let mut rankings = Vec::with_capacity(dataset.queries.len());
    for query in &dataset.queries {
        let (status, payload) = request_json(client, "POST", &format!("{url}/api/query"), Some(&json!({"query": query.text, "top_k": dataset.cutoffs.iter().copied().max().unwrap_or(1)}))).await?;
        check(
            status == 200 && payload.is_object(),
            format!("query {} returned HTTP {status}: {payload}", query.id),
        )?;
        check(
            payload.get("grounding") == Some(&json!("grounded")),
            format!("query {} was not grounded: {payload}", query.id),
        )?;
        check(
            payload
                .get("answer")
                .and_then(Value::as_str)
                .is_some_and(|answer| !answer.trim().is_empty()),
            format!("query {} returned an empty answer", query.id),
        )?;
        let chunks = payload
            .get("chunks")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Message(format!("query {} returned no chunks", query.id)))?;
        let ranked = chunks
            .iter()
            .map(|chunk| {
                chunk
                    .get("chunkId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        Error::Message(format!("query {} returned an invalid chunk", query.id))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let citation_ids = payload
            .get("citations")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::Message(format!("query {} returned invalid citations", query.id))
            })?;
        let mut citation_values = citation_ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        citation_values.sort_unstable();
        let mut ranked_values = ranked.clone();
        ranked_values.sort_unstable();
        check(
            citation_values == ranked_values,
            format!("query {} returned invalid citations", query.id),
        )?;
        rankings.push(ranked);
    }
    let metrics = evaluate(&rankings, dataset)?;
    let failures = metrics
        .iter()
        .filter_map(|(name, value)| {
            let variable = format!(
                "OHARA_QUALITY_MIN_{}",
                name.to_ascii_uppercase().replace('@', "_AT_")
            );
            let minimum = std::env::var(&variable)
                .ok()
                .map(|value| value.parse::<f64>())
                .transpose()
                .ok()
                .flatten()
                .unwrap_or(1.0);
            (value < &minimum).then(|| format!("{name}={value:.3} is below {minimum:.3}"))
        })
        .collect::<Vec<_>>();
    check(failures.is_empty(), failures.join("; "))?;
    println!("retrieval quality: {} golden queries", rankings.len());
    for (name, value) in metrics {
        println!("  {name}={value:.3}");
    }
    println!("retrieval quality passed: all configured minimums satisfied");
    Ok(())
}

fn evaluate(rankings: &[Vec<String>], dataset: &Dataset) -> Result<BTreeMap<String, f64>> {
    check(
        rankings.len() == dataset.queries.len(),
        "ranking count does not match query count",
    )?;
    let mut metrics = BTreeMap::new();
    for cutoff in &dataset.cutoffs {
        let value = rankings
            .iter()
            .zip(&dataset.queries)
            .map(|(ranking, query)| recall_at_k(ranking, &relevance_pairs(query), *cutoff))
            .sum::<f64>()
            / count_as_f64(rankings.len());
        metrics.insert(format!("recall@{cutoff}"), value);
    }
    metrics.insert(
        "mrr".to_owned(),
        rankings
            .iter()
            .zip(&dataset.queries)
            .map(|(ranking, query)| mean_reciprocal_rank(ranking, &relevance_pairs(query)))
            .sum::<f64>()
            / count_as_f64(rankings.len()),
    );
    for cutoff in &dataset.cutoffs {
        let value = rankings
            .iter()
            .zip(&dataset.queries)
            .map(|(ranking, query)| ndcg_at_k(ranking, &relevance_pairs(query), *cutoff))
            .sum::<f64>()
            / count_as_f64(rankings.len());
        metrics.insert(format!("ndcg@{cutoff}"), value);
    }
    Ok(metrics)
}

fn relevance_pairs(query: &Query) -> Vec<(String, i64)> {
    query
        .relevance
        .iter()
        .map(|(id, grade)| (id.clone(), *grade))
        .collect()
}

fn count_as_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::MAX, f64::from)
}

fn validate_dataset(dataset: &Dataset) -> Result<()> {
    check(
        dataset.schema_version == 1,
        "retrieval golden dataset must use schema version 1",
    )?;
    check(
        !dataset.cutoffs.is_empty() && dataset.cutoffs.iter().all(|cutoff| *cutoff > 0),
        "retrieval golden dataset must define positive cutoffs",
    )?;
    check(
        !dataset.documents.is_empty() && !dataset.queries.is_empty(),
        "retrieval golden dataset must define documents and queries",
    )?;
    let ids = dataset
        .documents
        .iter()
        .map(|document| document.chunk_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    check(
        ids.len() == dataset.documents.len()
            && dataset
                .documents
                .iter()
                .all(|document| !document.chunk_id.is_empty() && !document.text.trim().is_empty()),
        "golden documents need unique ids and non-empty text",
    )?;
    let mut query_ids = std::collections::HashSet::new();
    for query in &dataset.queries {
        check(
            !query.id.is_empty()
                && query_ids.insert(query.id.as_str())
                && !query.text.trim().is_empty()
                && !query.relevance.is_empty(),
            "golden queries need unique ids, text, and relevance",
        )?;
        check(
            query.relevance.keys().all(|id| ids.contains(id.as_str()))
                && query
                    .relevance
                    .values()
                    .all(|grade| (0..=3).contains(grade))
                && query.relevance.values().any(|grade| *grade > 0),
            "golden relevance must reference documents with grades 0..3 and contain a relevant document",
        )?;
    }
    check(
        dataset
            .documents
            .iter()
            .all(|document| deterministic_embedding(&document.text).len() == EMBEDDING_DIMENSION),
        "deterministic embedding dimension changed",
    )
}
