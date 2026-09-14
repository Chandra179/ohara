//! Live Qdrant exact-search versus HNSW benchmark.

use crate::http::{client, request_json, require_success};
use crate::metrics::{percentile, recall_at_k};
use crate::{Error, Result, check, positive_int, repository_root};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const DEFAULT_QDRANT_URL: &str = "http://127.0.0.1:6335";
const DEFAULT_REPETITIONS: usize = 10;
const BENCHMARK_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    vector_dimension: usize,
    corpus_size: usize,
    query_count: usize,
    relevant_per_query: usize,
    top_k: usize,
    hnsw: HnswSettings,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HnswSettings {
    m: usize,
    ef_construct: usize,
    ef_search: usize,
}

struct Query {
    vector: Vec<f64>,
    relevant: Vec<String>,
}

/// Execute the real Qdrant exact/HNSW comparison.
///
/// # Errors
///
/// Returns an error when the fixture is invalid, Docker or Qdrant is
/// unavailable, or either collection produces an invalid response.
pub async fn run() -> Result<Value> {
    let root = repository_root()?;
    let fixture_path = root.join("docs/architecture/fixtures/qdrant-hnsw-v1.json");
    let fixture: Fixture = serde_json::from_slice(
        &std::fs::read(&fixture_path)
            .map_err(|source| Error::io("read Qdrant HNSW fixture", source))?,
    )
    .map_err(|source| Error::Json {
        context: fixture_path.display().to_string(),
        source,
    })?;
    validate_fixture(&fixture)?;
    let repetitions = positive_int("OHARA_HNSW_BENCHMARK_REPETITIONS", DEFAULT_REPETITIONS)?;
    let base_url = std::env::var("OHARA_HNSW_BENCHMARK_QDRANT_URL")
        .unwrap_or_else(|_| DEFAULT_QDRANT_URL.to_owned());
    let container_id = qdrant_container_id(&root)?
        .ok_or_else(|| Error::Message("Qdrant container was not found; start it with `make providers` or set OHARA_HNSW_BENCHMARK_CONTAINER".to_owned()))?;
    let (points, queries) = build_workload(&fixture)?;
    let suffix = std::process::id();
    let collections = [
        ("exact", format!("ohara_hnsw_benchmark_{suffix}_exact")),
        ("hnsw", format!("ohara_hnsw_benchmark_{suffix}_hnsw")),
    ];
    let http_client = client()?;
    let mut results = serde_json::Map::new();
    let benchmark_result: Result<()> = async {
        for (mode, collection) in &collections {
            delete_collection(&http_client, &base_url, collection).await?;
            results.insert(
                mode.to_string(),
                run_mode(ModeBenchmark {
                    client: &http_client,
                    base_url: &base_url,
                    mode,
                    collection,
                    points: &points,
                    queries: &queries,
                    fixture: &fixture,
                    repetitions,
                    container_id: &container_id,
                })
                .await?,
            );
        }
        let exact = results.get("exact").ok_or_else(|| Error::Message("exact benchmark result missing".to_owned()))?;
        let hnsw = results.get("hnsw").ok_or_else(|| Error::Message("HNSW benchmark result missing".to_owned()))?;
        let exact_recall = exact.get("recall@10").and_then(Value::as_f64).unwrap_or(0.0);
        let hnsw_recall = hnsw.get("recall@10").and_then(Value::as_f64).unwrap_or(0.0);
        let exact_p95 = exact.get("queryP95Ms").and_then(Value::as_f64).ok_or_else(|| Error::Message("exact p95 result missing".to_owned()))?;
        let hnsw_p95 = hnsw.get("queryP95Ms").and_then(Value::as_f64).ok_or_else(|| Error::Message("HNSW p95 result missing".to_owned()))?;
        let ratio = if exact_recall == 0.0 { 0.0 } else { hnsw_recall / exact_recall };
        let recall_gate = ratio >= 0.98;
        let latency_gate = hnsw_p95 < exact_p95;
        let adopted = recall_gate && latency_gate;
        results.insert("comparison".to_owned(), json!({
            "recallAt10Ratio": round(ratio, 6),
            "recallGate": recall_gate,
            "p95LatencyImproved": latency_gate,
            "adoptHnsw": adopted,
            "recommendedMode": if adopted { "hnsw" } else { "exact" },
            "reason": if adopted { "HNSW meets the recall@10 and p95 latency gates" } else { "keep exact: HNSW must retain at least 98% recall@10 and improve p95 latency" },
        }));
        Ok(())
    }.await;
    for (_, collection) in &collections {
        if let Err(error) = delete_collection(&http_client, &base_url, collection).await {
            eprintln!("could not remove benchmark collection {collection}: {error}");
        }
    }
    benchmark_result?;
    let result = json!({
        "schemaVersion": BENCHMARK_SCHEMA_VERSION,
        "fixture": fixture_path.strip_prefix(&root).map_or_else(|_| fixture_path.display().to_string(), |path| path.display().to_string()),
        "qdrantUrl": base_url,
        "corpusSize": fixture.corpus_size,
        "queryCount": fixture.query_count,
        "topK": fixture.top_k,
        "repetitions": repetitions,
        "exact": results.remove("exact").unwrap_or(Value::Null),
        "hnsw": results.remove("hnsw").unwrap_or(Value::Null),
        "comparison": results.remove("comparison").unwrap_or(Value::Null),
    });
    Ok(result)
}

/// Print the human-readable comparison report.
///
/// # Errors
///
/// Returns an error when a required benchmark field is missing.
pub fn print_report(result: &Value) -> Result<()> {
    let corpus_size = result
        .get("corpusSize")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let query_count = result
        .get("queryCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let top_k = result.get("topK").and_then(Value::as_u64).unwrap_or(0);
    let repetitions = result
        .get("repetitions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!(
        "Qdrant HNSW benchmark (corpus={corpus_size}, queries={query_count}, top-k={top_k}, repetitions={repetitions}):"
    );
    println!("mode   recall@1  recall@3  recall@5  recall@10  build_ms  p50_ms  p95_ms  peak_mib");
    for mode in ["exact", "hnsw"] {
        let item = result
            .get(mode)
            .ok_or_else(|| Error::Message(format!("{mode} result missing")))?;
        println!(
            "{mode:<6} {:.3}     {:.3}     {:.3}     {:.3}      {:.2}    {:.2}   {:.2}   {:.2}",
            number(item, "recall@1")?,
            number(item, "recall@3")?,
            number(item, "recall@5")?,
            number(item, "recall@10")?,
            number(item, "buildMs")?,
            number(item, "queryP50Ms")?,
            number(item, "queryP95Ms")?,
            number(item, "peakMemoryMiB")?
        );
    }
    let comparison = result
        .get("comparison")
        .ok_or_else(|| Error::Message("comparison result missing".to_owned()))?;
    println!(
        "decision: {} (recall@10 ratio={:.3}, recall gate={}, p95 gate={})",
        comparison
            .get("recommendedMode")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        number(comparison, "recallAt10Ratio")?,
        if comparison.get("recallGate") == Some(&json!(true)) {
            "pass"
        } else {
            "fail"
        },
        if comparison.get("p95LatencyImproved") == Some(&json!(true)) {
            "pass"
        } else {
            "fail"
        }
    );
    println!(
        "reason: {}",
        comparison
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    Ok(())
}

fn validate_fixture(fixture: &Fixture) -> Result<()> {
    check(
        fixture.schema_version == BENCHMARK_SCHEMA_VERSION,
        "Qdrant HNSW fixture must use schema version 1",
    )?;
    check(
        fixture.vector_dimension > 0
            && fixture.corpus_size > 0
            && fixture.query_count > 0
            && fixture.relevant_per_query > 0
            && fixture.top_k > 0
            && fixture.hnsw.m > 0
            && fixture.hnsw.ef_construct > 0
            && fixture.hnsw.ef_search > 0,
        "Qdrant HNSW fixture counts and settings must be positive",
    )?;
    check(
        fixture.relevant_per_query * fixture.query_count <= fixture.corpus_size,
        "Qdrant HNSW fixture has more relevant points than corpus capacity",
    )?;
    check(
        fixture.top_k <= fixture.corpus_size,
        "Qdrant HNSW top-k must fit inside the corpus",
    )
}

fn build_workload(fixture: &Fixture) -> Result<(Vec<Value>, Vec<Query>)> {
    let seed = "ohara-qdrant-hnsw-v1";
    let mut points = Vec::with_capacity(fixture.corpus_size);
    let mut queries = Vec::with_capacity(fixture.query_count);
    let mut used = 0;
    for query_index in 0..fixture.query_count {
        let prototype = digest_vector(
            seed,
            &format!("topic:{query_index}"),
            fixture.vector_dimension,
        )?;
        let mut relevant = Vec::with_capacity(fixture.relevant_per_query);
        for relevant_index in 0..fixture.relevant_per_query {
            let logical_id = format!("topic-{query_index:02}-gold-{relevant_index:02}");
            let vector = blend(
                &prototype,
                &digest_vector(
                    seed,
                    &format!("noise:{logical_id}"),
                    fixture.vector_dimension,
                )?,
                0.02,
            )?;
            points.push(point(&logical_id, &vector));
            relevant.push(logical_id);
            used += 1;
        }
        queries.push(Query {
            vector: blend(
                &prototype,
                &digest_vector(
                    seed,
                    &format!("query-noise:{query_index}"),
                    fixture.vector_dimension,
                )?,
                0.01,
            )?,
            relevant,
        });
    }
    for index in 0..fixture.corpus_size - used {
        let logical_id = format!("background-{index:04}");
        let vector = digest_vector(
            seed,
            &format!("background:{index}"),
            fixture.vector_dimension,
        )?;
        points.push(point(&logical_id, &vector));
    }
    check(
        points.len() == fixture.corpus_size,
        "generated corpus size does not match fixture",
    )?;
    Ok((points, queries))
}

fn digest_vector(seed: &str, label: &str, dimension: usize) -> Result<Vec<f64>> {
    let mut values = Vec::with_capacity(dimension);
    let mut counter = 0_u64;
    while values.len() < dimension {
        let digest = Sha256::digest(format!("{seed}:{label}:{counter}").as_bytes());
        values.extend(digest.iter().map(|byte| (f64::from(*byte) - 127.5) / 127.5));
        counter += 1;
    }
    let vector = values.into_iter().take(dimension).collect::<Vec<_>>();
    normalize(vector)
}

fn blend(left: &[f64], right: &[f64], weight: f64) -> Result<Vec<f64>> {
    normalize(
        left.iter()
            .zip(right)
            .map(|(left, right)| left + weight * right)
            .collect(),
    )
}

fn normalize(vector: Vec<f64>) -> Result<Vec<f64>> {
    let norm = vector.iter().map(|value| value * value).sum::<f64>().sqrt();
    check(norm > 0.0, "generated vector must have a non-zero norm")?;
    Ok(vector.into_iter().map(|value| value / norm).collect())
}

fn point(logical_id: &str, vector: &[f64]) -> Value {
    let digest = Sha256::digest(logical_id.as_bytes());
    let hex = digest.iter().fold(String::new(), |mut output, byte| {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
        output
    });
    let id = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    json!({"id": id, "vector": vector, "payload": {"chunkId": logical_id, "text": logical_id}})
}

struct ModeBenchmark<'a> {
    client: &'a reqwest::Client,
    base_url: &'a str,
    mode: &'a str,
    collection: &'a str,
    points: &'a [Value],
    queries: &'a [Query],
    fixture: &'a Fixture,
    repetitions: usize,
    container_id: &'a str,
}

async fn run_mode(config: ModeBenchmark<'_>) -> Result<Value> {
    let ModeBenchmark {
        client,
        base_url,
        mode,
        collection,
        points,
        queries,
        fixture,
        repetitions,
        container_id,
    } = config;
    let mut samples = vec![memory_bytes(container_id)];
    let build_started = Instant::now();
    create_collection(client, base_url, collection, mode, fixture).await?;
    check(
        collection_dimension(client, base_url, collection).await? == fixture.vector_dimension,
        format!("{mode} collection has an incompatible vector dimension"),
    )?;
    upsert(client, base_url, collection, points).await?;
    wait_for_collection(client, base_url, collection).await?;
    let build_ms = build_started.elapsed().as_secs_f64() * 1_000.0;
    samples.push(memory_bytes(container_id));
    for query in queries {
        let _ = search(
            client,
            base_url,
            collection,
            query,
            mode,
            fixture.top_k,
            fixture.hnsw.ef_search,
        )
        .await?;
    }
    let mut timings = Vec::with_capacity(repetitions * queries.len());
    let mut rankings = Vec::with_capacity(queries.len());
    for repetition in 0..repetitions {
        for query in queries {
            let started = Instant::now();
            let ranked = search(
                client,
                base_url,
                collection,
                query,
                mode,
                fixture.top_k,
                fixture.hnsw.ef_search,
            )
            .await?;
            timings.push(started.elapsed().as_secs_f64() * 1_000.0);
            if repetition == 0 {
                rankings.push(ranked);
            }
        }
        samples.push(memory_bytes(container_id));
    }
    let cutoffs = [1, 3, 5, 10];
    let recalls = cutoffs
        .into_iter()
        .map(|cutoff| {
            let value = rankings
                .iter()
                .zip(queries)
                .map(|(ranking, query)| {
                    let relevance = query
                        .relevant
                        .iter()
                        .map(|id| (id.clone(), 1))
                        .collect::<Vec<_>>();
                    recall_at_k(ranking, &relevance, cutoff)
                })
                .sum::<f64>()
                / count_as_f64(queries.len());
            (format!("recall@{cutoff}"), json!(round(value, 6)))
        })
        .collect::<serde_json::Map<_, _>>();
    let observed = samples.into_iter().flatten().collect::<Vec<_>>();
    check(
        !observed.is_empty(),
        "could not sample Qdrant container memory",
    )?;
    let peak = observed.into_iter().max().unwrap_or(0);
    let mut result = serde_json::Map::new();
    result.insert("buildMs".to_owned(), json!(build_ms));
    result.insert("queryP50Ms".to_owned(), json!(percentile(&timings, 50.0)?));
    result.insert("queryP95Ms".to_owned(), json!(percentile(&timings, 95.0)?));
    result.insert("peakMemoryBytes".to_owned(), json!(peak));
    result.insert(
        "peakMemoryMiB".to_owned(),
        json!(round(bytes_to_mib(peak), 2)),
    );
    result.extend(recalls);
    Ok(Value::Object(result))
}

async fn create_collection(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
    mode: &str,
    fixture: &Fixture,
) -> Result<()> {
    let threshold = if mode == "exact" { 1_000_000_000 } else { 10 };
    let (status, payload) = request_json(client, "PUT", &collection_url(base_url, name), Some(&json!({"vectors": {"size": fixture.vector_dimension, "distance": "Cosine"}, "hnsw_config": {"m": fixture.hnsw.m, "ef_construct": fixture.hnsw.ef_construct, "full_scan_threshold": threshold}}))).await?;
    require_success(status, &payload, "create Qdrant collection")
}

async fn collection_dimension(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
) -> Result<usize> {
    let (status, payload) =
        request_json(client, "GET", &collection_url(base_url, name), None).await?;
    require_success(status, &payload, "read Qdrant collection")?;
    payload
        .pointer("/result/config/params/vectors/size")
        .and_then(Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .map_err(|_| Error::Message("Qdrant collection dimension exceeds usize".to_owned()))?
        .ok_or_else(|| {
            Error::Message("Qdrant collection has no single vector dimension".to_owned())
        })
}

async fn upsert(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
    points: &[Value],
) -> Result<()> {
    let (status, payload) = request_json(
        client,
        "PUT",
        &format!("{}?wait=true", collection_url(base_url, name) + "/points"),
        Some(&json!({"points": points})),
    )
    .await?;
    require_success(status, &payload, "upsert Qdrant points")
}

async fn wait_for_collection(client: &reqwest::Client, base_url: &str, name: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (status, payload) =
            request_json(client, "GET", &collection_url(base_url, name), None).await?;
        require_success(status, &payload, "read Qdrant collection state")?;
        if payload.pointer("/result/status").and_then(Value::as_str) == Some("green")
            && payload
                .pointer("/result/optimizer_status")
                .and_then(Value::as_str)
                .is_none_or(|value| value == "ok")
        {
            return Ok(());
        }
        check(
            tokio::time::Instant::now() < deadline,
            format!("timed out waiting for Qdrant collection {name}"),
        )?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn search(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
    query: &Query,
    mode: &str,
    top_k: usize,
    ef_search: usize,
) -> Result<Vec<String>> {
    let params = if mode == "exact" {
        json!({"exact": true})
    } else {
        json!({"exact": false, "hnsw_ef": ef_search})
    };
    let (status, payload) = request_json(client, "POST", &format!("{}{}", collection_url(base_url, name), "/points/search"), Some(&json!({"vector": query.vector, "limit": top_k, "with_payload": true, "params": params}))).await?;
    require_success(status, &payload, "search Qdrant collection")?;
    let result = payload
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Message("Qdrant search response has no result list".to_owned()))?;
    result
        .iter()
        .map(|item| {
            item.pointer("/payload/chunkId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::Message("Qdrant result is missing payload.chunkId".to_owned())
                })
        })
        .collect()
}

async fn delete_collection(client: &reqwest::Client, base_url: &str, name: &str) -> Result<()> {
    let (status, payload) =
        request_json(client, "DELETE", &collection_url(base_url, name), None).await?;
    if status == 404 {
        return Ok(());
    }
    require_success(status, &payload, "delete Qdrant collection")
}

fn collection_url(base_url: &str, name: &str) -> String {
    format!("{}/collections/{name}", base_url.trim_end_matches('/'))
}

fn memory_bytes(container_id: &str) -> Option<u64> {
    let output = Command::new("docker")
        .args([
            "stats",
            "--no-stream",
            "--format",
            "{{.MemUsage}}",
            container_id,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let output_text = String::from_utf8_lossy(&output.stdout);
    let value = output_text.trim().split('/').next()?.trim();
    for (unit, multiplier) in [
        ("GiB", 1024_u64.pow(3)),
        ("MiB", 1024_u64.pow(2)),
        ("KiB", 1024_u64),
        ("GB", 1000_u64.pow(3)),
        ("MB", 1000_u64.pow(2)),
        ("kB", 1000_u64),
        ("B", 1),
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            let multiplier = f64::from(u32::try_from(multiplier).unwrap_or(u32::MAX));
            return number
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .and_then(|value| (value * multiplier).round().to_string().parse::<u64>().ok());
        }
    }
    None
}

fn qdrant_container_id(root: &Path) -> Result<Option<String>> {
    if let Some(configured) = std::env::var_os("OHARA_HNSW_BENCHMARK_CONTAINER") {
        return Ok(Some(configured.to_string_lossy().into_owned()));
    }
    let output = Command::new("docker")
        .args(["compose", "ps", "-q", "qdrant"])
        .current_dir(root)
        .output()
        .map_err(|source| Error::io("find Qdrant container", source))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::to_owned))
}

fn number(value: &Value, field: &str) -> Result<f64> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| Error::Message(format!("benchmark result missing {field}")))
}

fn round(value: f64, decimals: i32) -> f64 {
    let scale = 10_f64.powi(decimals);
    (value * scale).round() / scale
}

fn count_as_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::MAX, f64::from)
}

fn bytes_to_mib(value: u64) -> f64 {
    let kib = value / 1024;
    f64::from(u32::try_from(kib).unwrap_or(u32::MAX)) / 1024.0
}

#[cfg(test)]
mod tests {
    use super::{Fixture, HnswSettings, round, validate_fixture};

    fn fixture() -> Fixture {
        Fixture {
            schema_version: 1,
            vector_dimension: 384,
            corpus_size: 512,
            query_count: 16,
            relevant_per_query: 4,
            top_k: 10,
            hnsw: HnswSettings {
                m: 16,
                ef_construct: 100,
                ef_search: 64,
            },
        }
    }

    #[test]
    fn rounds_metrics_for_machine_output() {
        assert!((round(1.23456, 3) - 1.235).abs() < f64::EPSILON);
    }

    #[test]
    fn validates_benchmark_fixture_configuration() {
        assert!(validate_fixture(&fixture()).is_ok());
    }

    #[test]
    fn rejects_benchmark_fixture_with_zero_corpus() {
        let mut invalid = fixture();
        invalid.corpus_size = 0;
        assert!(validate_fixture(&invalid).is_err());
    }
}
