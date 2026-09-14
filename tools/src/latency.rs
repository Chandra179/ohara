//! Cold-start and warm-query latency benchmark.

use crate::artifacts::temporary_directory;
use crate::http::{client, free_port, request_json, wait_for_http};
use crate::metrics::percentile;
use crate::process::{ManagedProcess, environment, with_values};
use crate::providers::{start_ollama_server, start_qdrant_server};
use crate::{Error, Result, check, positive_float, positive_int, repository_root};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Execute the deterministic cold and warm latency benchmark.
///
/// # Errors
///
/// Returns an error when a process, fixture provider, query, or configured
/// latency threshold fails.
pub async fn run() -> Result<()> {
    let config = Config::from_environment()?;
    let root = repository_root()?;
    let temporary = temporary_directory("ohara-benchmark")?;
    let data_dir = temporary.join("data");
    let log_dir = temporary.join("logs");
    create_directories(&data_dir, &log_dir)?;
    let http_client = client()?;
    let mut qdrant = start_qdrant_server(None).await?;
    let mut ollama = start_ollama_server().await?;
    qdrant.add_points(fixture_points()).await;
    let environment = base_environment(&data_dir, qdrant.port(), ollama.port());
    let result = run_benchmark(&config, &root, &http_client, &log_dir, &environment).await;
    qdrant.shutdown().await?;
    ollama.shutdown().await?;
    let _ = std::fs::remove_dir_all(&temporary);
    result
}

#[derive(Clone, Copy)]
struct Config {
    cold_runs: usize,
    warm_runs: usize,
    cold_p50_limit: f64,
    cold_p95_limit: f64,
    warm_p50_limit: f64,
    warm_p95_limit: f64,
}

impl Config {
    fn from_environment() -> Result<Self> {
        Ok(Self {
            cold_runs: positive_int("OHARA_BENCHMARK_COLD_RUNS", 5)?,
            warm_runs: positive_int("OHARA_BENCHMARK_WARM_RUNS", 20)?,
            cold_p50_limit: positive_float("OHARA_BENCHMARK_COLD_P50_MS", 1_000.0)?,
            cold_p95_limit: positive_float("OHARA_BENCHMARK_COLD_P95_MS", 3_000.0)?,
            warm_p50_limit: positive_float("OHARA_BENCHMARK_WARM_P50_MS", 250.0)?,
            warm_p95_limit: positive_float("OHARA_BENCHMARK_WARM_P95_MS", 500.0)?,
        })
    }
}

async fn run_benchmark(
    config: &Config,
    root: &std::path::Path,
    http_client: &reqwest::Client,
    log_dir: &std::path::Path,
    environment: &HashMap<String, String>,
) -> Result<()> {
    let mut cold_totals = Vec::with_capacity(config.cold_runs);
    let mut cold_starts = Vec::with_capacity(config.cold_runs);
    let mut cold_queries = Vec::with_capacity(config.cold_runs);
    let mut warm_queries = Vec::with_capacity(config.warm_runs);
    for run in 0..config.cold_runs {
        let result = cold_run(
            root,
            http_client,
            log_dir,
            environment,
            run,
            config.warm_runs,
        )
        .await?;
        cold_starts.push(result.startup_ms);
        cold_queries.push(result.query_ms);
        cold_totals.push(result.total_ms);
        if let Some(warm) = result.warm_queries {
            warm_queries = warm;
        }
    }
    let report =
        LatencyReport::from_samples(&cold_totals, &cold_starts, &cold_queries, &warm_queries)?;
    report.print();
    report.check(config)
}

struct ColdRun {
    startup_ms: f64,
    query_ms: f64,
    total_ms: f64,
    warm_queries: Option<Vec<f64>>,
}

async fn cold_run(
    root: &std::path::Path,
    http_client: &reqwest::Client,
    log_dir: &std::path::Path,
    base_environment: &HashMap<String, String>,
    run: usize,
    warm_runs: usize,
) -> Result<ColdRun> {
    let retrieval_port = free_port()?;
    let environment = with_values(
        base_environment,
        [(
            "OHARA_RETRIEVAL_BIND".to_owned(),
            format!("127.0.0.1:{retrieval_port}"),
        )],
    );
    let started = Instant::now();
    let mut process = ManagedProcess::start(
        root,
        format!("retrieval-cold-{run}"),
        "ohara-retrieval",
        &environment,
        log_dir,
    )?;
    let retrieval_url = format!("http://127.0.0.1:{retrieval_port}");
    let result = measure_cold(http_client, &retrieval_url, started, warm_runs).await;
    let stop_result = process.stop().await;
    match (result, stop_result) {
        (Ok(run), Ok(())) => Ok(run),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

async fn measure_cold(
    http_client: &reqwest::Client,
    retrieval_url: &str,
    started: Instant,
    warm_runs: usize,
) -> Result<ColdRun> {
    wait_for_http(
        http_client,
        &format!("{retrieval_url}/api/health"),
        None,
        Duration::from_secs(15),
    )
    .await?;
    let startup_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let query_ms = query_once(http_client, retrieval_url).await?;
    let warm_queries = if warm_runs > 0 {
        let mut samples = Vec::with_capacity(warm_runs);
        for _ in 0..warm_runs {
            samples.push(query_once(http_client, retrieval_url).await?);
        }
        Some(samples)
    } else {
        None
    };
    Ok(ColdRun {
        startup_ms,
        query_ms,
        total_ms: startup_ms + query_ms,
        warm_queries,
    })
}

struct LatencyReport {
    cold_p50: f64,
    cold_p95: f64,
    warm_p50: f64,
    warm_p95: f64,
    startup_p50: f64,
    startup_p95: f64,
    query_p50: f64,
    query_p95: f64,
}

impl LatencyReport {
    fn from_samples(
        cold_totals: &[f64],
        cold_starts: &[f64],
        cold_queries: &[f64],
        warm_queries: &[f64],
    ) -> Result<Self> {
        Ok(Self {
            cold_p50: percentile(cold_totals, 50.0)?,
            cold_p95: percentile(cold_totals, 95.0)?,
            warm_p50: percentile(warm_queries, 50.0)?,
            warm_p95: percentile(warm_queries, 95.0)?,
            startup_p50: percentile(cold_starts, 50.0)?,
            startup_p95: percentile(cold_starts, 95.0)?,
            query_p50: percentile(cold_queries, 50.0)?,
            query_p95: percentile(cold_queries, 95.0)?,
        })
    }

    fn print(&self) {
        println!(
            "latency benchmark: cold total p50={:.1}ms p95={:.1}ms; warm query p50={:.1}ms p95={:.1}ms",
            self.cold_p50, self.cold_p95, self.warm_p50, self.warm_p95
        );
        println!(
            "latency detail: cold startup p50={:.1}ms p95={:.1}ms; cold query p50={:.1}ms p95={:.1}ms",
            self.startup_p50, self.startup_p95, self.query_p50, self.query_p95
        );
    }

    fn check(&self, config: &Config) -> Result<()> {
        let checks = [
            (self.cold_p50, config.cold_p50_limit, "cold p50"),
            (self.cold_p95, config.cold_p95_limit, "cold p95"),
            (self.warm_p50, config.warm_p50_limit, "warm p50"),
            (self.warm_p95, config.warm_p95_limit, "warm p95"),
        ];
        let failures = checks
            .into_iter()
            .filter(|(actual, limit, _)| actual > limit)
            .map(|(actual, limit, name)| format!("{name} {actual:.1}ms exceeds {limit:.1}ms"))
            .collect::<Vec<_>>();
        check(failures.is_empty(), failures.join("; "))?;
        println!("latency benchmark passed: all p50/p95 thresholds satisfied");
        Ok(())
    }
}

async fn query_once(client: &reqwest::Client, url: &str) -> Result<f64> {
    let started = Instant::now();
    let (status, payload) = request_json(
        client,
        "POST",
        &format!("{url}/api/query"),
        Some(&json!({"query": "What is Ohara?", "top_k": 5})),
    )
    .await?;
    let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
    check(
        status == 200,
        format!("query returned HTTP {status}: {payload}"),
    )?;
    check(
        payload.get("grounding") == Some(&json!("grounded")),
        format!("query was not grounded: {payload}"),
    )?;
    check(
        payload
            .get("answer")
            .and_then(Value::as_str)
            .is_some_and(|answer| !answer.trim().is_empty()),
        format!("query returned an empty answer: {payload}"),
    )?;
    Ok(elapsed)
}

fn fixture_points() -> impl Iterator<Item = Value> {
    (0..32).map(|index| {
        json!({
            "id": format!("fixture-point-{index}"),
            "vector": vec![0.0; 384],
            "payload": {
                "chunkId": format!("fixture-chunk-{index}"),
                "text": "Ohara is a private local knowledge base that turns web topics into searchable evidence.",
            },
        })
    })
}

fn base_environment(
    data_dir: &std::path::Path,
    qdrant_port: u16,
    ollama_port: u16,
) -> HashMap<String, String> {
    environment([
        ("OHARA_DATA_DIR".to_owned(), data_dir.display().to_string()),
        (
            "OHARA_QDRANT_URL".to_owned(),
            format!("http://127.0.0.1:{qdrant_port}"),
        ),
        (
            "OHARA_FALKORDB_URL".to_owned(),
            "redis://127.0.0.1:1".to_owned(),
        ),
        (
            "OHARA_LLM_URL".to_owned(),
            format!("http://127.0.0.1:{ollama_port}"),
        ),
        ("OHARA_LLM_MODEL".to_owned(), "fixture".to_owned()),
        (
            "OHARA_EMBEDDING_MODE".to_owned(),
            "deterministic".to_owned(),
        ),
    ])
}

fn create_directories(data_dir: &std::path::Path, log_dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(data_dir).map_err(|source| Error::io("create latency data", source))?;
    std::fs::create_dir_all(log_dir).map_err(|source| Error::io("create latency logs", source))
}
