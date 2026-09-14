//! Peak RSS benchmark for each Ohara process.

use crate::artifacts::{json_files, seed_directories, temporary_directory};
use crate::http::{client, free_port, request_json, wait_for_http};
use crate::process::{ManagedProcess, environment, peak_rss_kib, with_values};
use crate::providers::{
    FakeRedisServer, start_benchmark_feed, start_ollama_server, start_qdrant_server,
};
use crate::workload::{CorpusDocument, build_corpus, seed_cleaning, seed_graph, seed_indexer};
use crate::{Error, Result, check, positive_int, repository_root};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;

const EMBEDDING_DIMENSION: usize = 384;
const DEFAULT_DOCUMENTS: usize = 8;
const MAX_DOCUMENTS: usize = 10;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const WORKLOAD_TIMEOUT: Duration = Duration::from_mins(1);

/// A measured process result.
#[derive(Clone, Debug)]
pub struct ResourceMeasurement {
    /// Process name.
    pub process: String,
    /// Peak resident set size in KiB.
    pub peak_rss_kib: u64,
    /// Description of the exercised workload.
    pub workload: String,
}

impl ResourceMeasurement {
    /// Return peak resident set size in mebibytes for human-readable output.
    #[must_use]
    pub fn peak_rss_mib(&self) -> f64 {
        f64::from(u32::try_from(self.peak_rss_kib).unwrap_or(u32::MAX)) / 1024.0
    }

    fn as_json(&self) -> Value {
        json!({
            "peakRssBytes": self.peak_rss_kib * 1024,
            "peakRssKiB": self.peak_rss_kib,
            "peakRssMiB": (self.peak_rss_mib() * 100.0).round() / 100.0,
            "workload": self.workload,
        })
    }
}

struct MeasuredProcess {
    process: ManagedProcess,
    peak_rss_kib: Arc<AtomicU64>,
    stop_sampler: Arc<AtomicBool>,
    sampler: Option<thread::JoinHandle<()>>,
}

impl MeasuredProcess {
    fn start(
        root: &Path,
        name: &str,
        binary: &str,
        environment: &std::collections::HashMap<String, String>,
        log_dir: &Path,
    ) -> Result<Self> {
        let process = ManagedProcess::start(root, name, binary, environment, log_dir)?;
        let peak_rss = Arc::new(AtomicU64::new(peak_rss_kib(process.pid())?.unwrap_or(0)));
        let stop_sampler = Arc::new(AtomicBool::new(false));
        let sample_pid = process.pid();
        let sample_peak = peak_rss.clone();
        let sample_stop = stop_sampler.clone();
        let sampler = thread::Builder::new()
            .name(format!("rss-{name}"))
            .spawn(move || {
                while !sample_stop.load(Ordering::Relaxed) {
                    if let Ok(Some(value)) = peak_rss_kib(sample_pid) {
                        sample_peak.fetch_max(value, Ordering::Relaxed);
                    }
                    thread::sleep(SAMPLE_INTERVAL);
                }
            })
            .map_err(|source| Error::io("start RSS sampler", source))?;
        Ok(Self {
            process,
            peak_rss_kib: peak_rss,
            stop_sampler,
            sampler: Some(sampler),
        })
    }

    async fn wait_until(
        &mut self,
        predicate: impl Fn() -> Result<bool>,
        description: &str,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + WORKLOAD_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if predicate()? {
                return Ok(());
            }
            if !self.process.is_running()? {
                return Err(Error::Process {
                    name: self.process.name.clone(),
                    message: format!(
                        "exited while waiting for {description}\n{}",
                        self.process.log()?
                    ),
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(Error::Message(format!(
            "timed out waiting for {} {description}",
            self.process.name
        )))
    }

    async fn stop(&mut self) -> Result<()> {
        let result = self.process.stop().await;
        self.stop_sampler.store(true, Ordering::Relaxed);
        if let Some(sampler) = self.sampler.take() {
            sampler
                .join()
                .map_err(|_| Error::Message("RSS sampler panicked".to_owned()))?;
        }
        result
    }

    fn measurement(&self, workload: &str) -> ResourceMeasurement {
        ResourceMeasurement {
            process: self.process.name.clone(),
            peak_rss_kib: self.peak_rss_kib.load(Ordering::Relaxed),
            workload: workload.to_owned(),
        }
    }
}

/// Execute the five-process peak RSS benchmark.
///
/// # Errors
///
/// Returns an error when Linux process statistics, a process workload, or a
/// configured model cache is unavailable.
pub async fn run() -> Result<Vec<ResourceMeasurement>> {
    check(
        Path::new("/proc/self/status").is_file(),
        "peak RSS benchmark requires Linux /proc process statistics",
    )?;
    let document_count = positive_int("OHARA_RESOURCE_BENCHMARK_DOCUMENTS", DEFAULT_DOCUMENTS)?;
    check(
        document_count <= MAX_DOCUMENTS,
        format!("OHARA_RESOURCE_BENCHMARK_DOCUMENTS must be at most {MAX_DOCUMENTS}"),
    )?;
    let documents = build_corpus(document_count);
    let root = repository_root()?;
    let log_dir = temporary_directory("ohara-resource-logs")?;
    let result = async {
        let measurements = vec![
            run_scraper(&root, &documents, &log_dir).await?,
            run_cleaning(&root, &documents, &log_dir).await?,
            run_indexer(&root, &documents, &log_dir).await?,
            run_graph(&root, &documents, &log_dir).await?,
            run_retrieval(&root, &documents, &log_dir).await?,
        ];
        if let Some(output) = std::env::var_os("OHARA_RESOURCE_BENCHMARK_OUTPUT") {
            let output_path = PathBuf::from(output);
            let processes = measurements
                .iter()
                .map(|item| (item.process.clone(), item.as_json()))
                .collect::<serde_json::Map<_, _>>();
            let value = json!({
                "documents": document_count,
                "embeddingMode": std::env::var("OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE").unwrap_or_else(|_| "deterministic".to_owned()),
                "processes": processes,
            });
            let body = serde_json::to_vec_pretty(&value).map_err(|source| Error::Json {
                context: output_path.display().to_string(),
                source,
            })?;
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| Error::io("create resource output directory", source))?;
            }
            std::fs::write(&output_path, [body.as_slice(), b"\n"].concat())
                .map_err(|source| Error::Output { path: output_path, source })?;
        }
        Ok(measurements)
    }
    .await;
    let _ = std::fs::remove_dir_all(&log_dir);
    result
}

fn base_environment(
    data_dir: &Path,
    qdrant_url: &str,
    falkordb_url: &str,
    llm_url: &str,
) -> std::collections::HashMap<String, String> {
    let embedding_mode = std::env::var("OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE")
        .unwrap_or_else(|_| "deterministic".to_owned());
    environment([
        ("OHARA_DATA_DIR".to_owned(), data_dir.display().to_string()),
        ("OHARA_QDRANT_URL".to_owned(), qdrant_url.to_owned()),
        ("OHARA_FALKORDB_URL".to_owned(), falkordb_url.to_owned()),
        (
            "OHARA_FALKORDB_GRAPH".to_owned(),
            "ohara-resource-benchmark".to_owned(),
        ),
        ("OHARA_LLM_URL".to_owned(), llm_url.to_owned()),
        ("OHARA_LLM_MODEL".to_owned(), "fixture".to_owned()),
        ("OHARA_EMBEDDING_MODE".to_owned(), embedding_mode),
    ])
}

async fn run_scraper(
    root: &Path,
    documents: &[CorpusDocument],
    log_dir: &Path,
) -> Result<ResourceMeasurement> {
    let temporary = temporary_directory("ohara-resource-scraper")?;
    let data_dir = temporary.join("data");
    let mut feed = start_benchmark_feed(documents.to_vec()).await?;
    let scraper_port = free_port()?;
    let config = temporary.join("scraper-config.yaml");
    std::fs::write(
        &config,
        format!(
            "bind: 127.0.0.1:{scraper_port}\nsearch:\n  provider: rss\n  url: http://127.0.0.1:{}/news\nfetch:\n  kind: http\n",
            feed.port()
        ),
    )
    .map_err(|source| Error::io("write scraper resource config", source))?;
    let mut process = MeasuredProcess::start(
        root,
        "scraper",
        "ohara-scraper",
        &with_values(
            &base_environment(
                &data_dir,
                "http://127.0.0.1:1",
                "redis://127.0.0.1:1",
                "http://127.0.0.1:1",
            ),
            [(
                "OHARA_SCRAPER_CONFIG".to_owned(),
                config.display().to_string(),
            )],
        ),
        log_dir,
    )?;
    let http_client = client()?;
    let result = async {
        wait_for_http(
            &http_client,
            &format!("http://127.0.0.1:{scraper_port}/health"),
            Some(204),
            Duration::from_secs(15),
        )
        .await?;
        let (status, payload) = request_json(
            &http_client,
            "POST",
            &format!("http://127.0.0.1:{scraper_port}/scrape"),
            Some(&json!({"topic": "representative resource corpus", "limit": documents.len()})),
        )
        .await?;
        check(
            status == 200,
            format!("scraper returned HTTP {status}: {payload}"),
        )?;
        check(
            payload.get("discovered") == Some(&json!(documents.len()))
                && payload.get("enqueued") == Some(&json!(documents.len())),
            format!("scraper did not enqueue the representative corpus: {payload}"),
        )?;
        process
            .wait_until(
                || {
                    Ok(json_files(&data_dir.join("raw"))
                        .is_ok_and(|files| files.len() >= documents.len()))
                },
                "raw artifacts",
            )
            .await
    }
    .await;
    let stop_result = process.stop().await;
    feed.shutdown().await?;
    let measurement = process.measurement("topic scrape and raw publication");
    let _ = std::fs::remove_dir_all(&temporary);
    result?;
    stop_result?;
    Ok(measurement)
}

async fn run_cleaning(
    root: &Path,
    documents: &[CorpusDocument],
    log_dir: &Path,
) -> Result<ResourceMeasurement> {
    let temporary = temporary_directory("ohara-resource-cleaning")?;
    let data_dir = temporary.join("data");
    seed_cleaning(&data_dir, documents)?;
    let mut process = MeasuredProcess::start(
        root,
        "cleaning",
        "ohara-cleaning",
        &base_environment(
            &data_dir,
            "http://127.0.0.1:1",
            "redis://127.0.0.1:1",
            "http://127.0.0.1:1",
        ),
        log_dir,
    )?;
    let result = process
        .wait_until(
            || {
                Ok(json_files(&data_dir.join("clean"))
                    .is_ok_and(|files| files.len() >= documents.len()))
            },
            "clean artifacts",
        )
        .await;
    let stop_result = process.stop().await;
    let measurement = process.measurement("representative raw corpus extraction");
    let _ = std::fs::remove_dir_all(&temporary);
    result?;
    stop_result?;
    Ok(measurement)
}

async fn run_indexer(
    root: &Path,
    documents: &[CorpusDocument],
    log_dir: &Path,
) -> Result<ResourceMeasurement> {
    let temporary = temporary_directory("ohara-resource-indexer")?;
    let data_dir = temporary.join("data");
    seed_indexer(&data_dir, documents)?;
    let embedding_mode = std::env::var("OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE")
        .unwrap_or_else(|_| "deterministic".to_owned());
    if embedding_mode != "deterministic" {
        seed_model_cache(&data_dir)?;
    }
    let mut qdrant = start_qdrant_server(None).await?;
    let mut process = MeasuredProcess::start(
        root,
        "indexer",
        "ohara-indexer",
        &base_environment(
            &data_dir,
            &format!("http://127.0.0.1:{}", qdrant.port()),
            "redis://127.0.0.1:1",
            "http://127.0.0.1:1",
        ),
        log_dir,
    )?;
    let result = async {
        process
            .wait_until(
                || {
                    Ok(json_files(&data_dir.join("indexed"))
                        .is_ok_and(|files| files.len() >= documents.len()))
                },
                "indexed artifacts",
            )
            .await?;
        check(
            qdrant.point_count().await >= documents.len(),
            "indexer did not publish representative vectors",
        )
    }
    .await;
    let stop_result = process.stop().await;
    qdrant.shutdown().await?;
    let measurement = process.measurement("representative clean corpus chunking and indexing");
    let _ = std::fs::remove_dir_all(&temporary);
    result?;
    stop_result?;
    Ok(measurement)
}

async fn run_graph(
    root: &Path,
    documents: &[CorpusDocument],
    log_dir: &Path,
) -> Result<ResourceMeasurement> {
    let temporary = temporary_directory("ohara-resource-graph")?;
    let data_dir = temporary.join("data");
    seed_graph(&data_dir, documents)?;
    let mut falkordb = FakeRedisServer::start().await?;
    let mut process = MeasuredProcess::start(
        root,
        "graph",
        "ohara-graph",
        &base_environment(
            &data_dir,
            "http://127.0.0.1:1",
            &format!("redis://127.0.0.1:{}", falkordb.port()),
            "http://127.0.0.1:1",
        ),
        log_dir,
    )?;
    let result =
        process
            .wait_until(
                || {
                    Ok(json_files(&data_dir.join("inbox/graph"))
                        .map_or(true, |files| files.is_empty())
                        && falkordb.query_count() >= documents.len())
                },
                "graph writes",
            )
            .await;
    let stop_result = process.stop().await;
    falkordb.shutdown().await?;
    let measurement = process.measurement("representative indexed corpus graph publication");
    let _ = std::fs::remove_dir_all(&temporary);
    result?;
    stop_result?;
    Ok(measurement)
}

async fn run_retrieval(
    root: &Path,
    documents: &[CorpusDocument],
    log_dir: &Path,
) -> Result<ResourceMeasurement> {
    let temporary = temporary_directory("ohara-resource-retrieval")?;
    let data_dir = temporary.join("data");
    seed_directories(&data_dir)?;
    let embedding_mode = std::env::var("OHARA_RESOURCE_BENCHMARK_EMBEDDING_MODE")
        .unwrap_or_else(|_| "deterministic".to_owned());
    if embedding_mode != "deterministic" {
        seed_model_cache(&data_dir)?;
    }
    let mut qdrant = start_qdrant_server(None).await?;
    let mut ollama = start_ollama_server().await?;
    let mut falkordb = FakeRedisServer::start().await?;
    let points = documents.iter().map(|document| {
        json!({
            "id": format!("{}-point", document.document_id),
            "vector": vec![0.0; EMBEDDING_DIMENSION],
            "payload": {
                "chunkId": format!("{}-chunk-0", document.document_id),
                "documentId": document.document_id,
                "title": document.title,
                "sourceUrl": document.source_url,
                "text": document.markdown,
            },
        })
    });
    qdrant.add_points(points).await;
    let retrieval_port = free_port()?;
    let mut process = MeasuredProcess::start(
        root,
        "retrieval",
        "ohara-retrieval",
        &with_values(
            &base_environment(
                &data_dir,
                &format!("http://127.0.0.1:{}", qdrant.port()),
                &format!("redis://127.0.0.1:{}", falkordb.port()),
                &format!("http://127.0.0.1:{}", ollama.port()),
            ),
            [(
                "OHARA_RETRIEVAL_BIND".to_owned(),
                format!("127.0.0.1:{retrieval_port}"),
            )],
        ),
        log_dir,
    )?;
    let http_client = client()?;
    let result = async {
        let url = format!("http://127.0.0.1:{retrieval_port}");
        wait_for_http(
            &http_client,
            &format!("{url}/api/health"),
            None,
            Duration::from_secs(15),
        )
        .await?;
        let (status, payload) = request_json(
            &http_client,
            "POST",
            &format!("{url}/api/query"),
            Some(&json!({"query": "What is Ohara?", "top_k": documents.len()})),
        )
        .await?;
        check(
            status == 200,
            format!("retrieval returned HTTP {status}: {payload}"),
        )?;
        check(
            payload.get("grounding") == Some(&json!("grounded"))
                && payload
                    .get("answer")
                    .and_then(Value::as_str)
                    .is_some_and(|answer| !answer.trim().is_empty()),
            format!("retrieval did not return a grounded answer: {payload}"),
        )
    }
    .await;
    let stop_result = process.stop().await;
    falkordb.shutdown().await?;
    qdrant.shutdown().await?;
    ollama.shutdown().await?;
    let measurement = process.measurement("representative ranked query and synthesis");
    let _ = std::fs::remove_dir_all(&temporary);
    result?;
    stop_result?;
    Ok(measurement)
}

fn seed_model_cache(data_dir: &Path) -> Result<()> {
    let source = PathBuf::from(
        std::env::var("OHARA_RESOURCE_BENCHMARK_MODEL_CACHE")
            .unwrap_or_else(|_| "data/models".to_owned()),
    );
    check(
        source.is_dir(),
        format!(
            "model-backed resource benchmark requires a model cache directory at {}; set OHARA_RESOURCE_BENCHMARK_MODEL_CACHE to an existing cache",
            source.display()
        ),
    )?;
    let target = data_dir.join("models");
    std::fs::remove_dir(&target)
        .map_err(|source| Error::io("remove empty model directory", source))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        source
            .canonicalize()
            .map_err(|source| Error::io("resolve model cache", source))?,
        &target,
    )
    .map_err(|source| Error::io("link model cache", source))?;
    #[cfg(not(unix))]
    {
        let _ = target;
        return Err(Error::Message(
            "model-backed resource benchmark requires a Unix host".to_owned(),
        ));
    }
    Ok(())
}
