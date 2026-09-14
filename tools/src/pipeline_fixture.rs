//! Real process-seam fixture for scrape-to-query verification.

use crate::artifacts::{read_json, temporary_directory};
use crate::http::{client, free_port, request_json, wait_for_http};
use crate::process::{ManagedProcess, environment, with_values};
use crate::providers::{
    FixtureServer, OllamaServer, QdrantServer, start_fixture_server, start_ollama_server,
    start_qdrant_server,
};
use crate::{Error, Result, check, repository_root};
use reqwest::Client;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROCESS_START_TIMEOUT: Duration = Duration::from_secs(15);
const PIPELINE_TIMEOUT: Duration = Duration::from_secs(30);

/// Execute the deterministic scrape-to-query fixture.
///
/// # Errors
///
/// Returns an error when a fixture provider, Ohara process, artifact, or
/// response contract fails validation.
pub async fn run() -> Result<()> {
    let mut setup = PipelineSetup::start().await?;
    match setup.execute().await {
        Ok(()) => setup.cleanup(true).await,
        Err(error) => {
            if let Err(cleanup_error) = setup.cleanup(false).await {
                eprintln!("pipeline fixture cleanup failed: {cleanup_error}");
            }
            Err(error)
        }
    }
}

struct PipelineSetup {
    root: PathBuf,
    temporary: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
    http_client: Client,
    fixture_server: FixtureServer,
    qdrant_server: QdrantServer,
    ollama_server: OllamaServer,
    scraper_port: u16,
    retrieval_port: u16,
    scraper_environment: HashMap<String, String>,
    base_environment: HashMap<String, String>,
    retrieval_environment: HashMap<String, String>,
    processes: Vec<ManagedProcess>,
}

impl PipelineSetup {
    async fn start() -> Result<Self> {
        let root = repository_root()?;
        let (temporary, data_dir, log_dir) = create_workspace()?;
        let http_client = client()?;
        let fixture_server = start_fixture_server().await?;
        let search_mode = pipeline_search_mode()?;
        let qdrant_server = start_qdrant_server(Some(&search_mode)).await?;
        let ollama_server = start_ollama_server().await?;
        let scraper_port = free_port()?;
        let retrieval_port = free_port()?;
        let scraper_config = write_scraper_config(&temporary, scraper_port, &fixture_server)?;
        let base_environment = base_environment(
            &data_dir,
            qdrant_server.port(),
            ollama_server.port(),
            &search_mode,
        );
        let scraper_environment = with_values(
            &base_environment,
            [("OHARA_SCRAPER_CONFIG".to_owned(), scraper_config)],
        );
        let retrieval_environment = with_values(
            &base_environment,
            [
                (
                    "OHARA_RETRIEVAL_BIND".to_owned(),
                    format!("127.0.0.1:{retrieval_port}"),
                ),
                (
                    "OHARA_SCRAPER_URL".to_owned(),
                    format!("http://127.0.0.1:{scraper_port}"),
                ),
            ],
        );
        Ok(Self {
            root,
            temporary,
            data_dir,
            log_dir,
            http_client,
            fixture_server,
            qdrant_server,
            ollama_server,
            scraper_port,
            retrieval_port,
            scraper_environment,
            base_environment,
            retrieval_environment,
            processes: Vec::new(),
        })
    }

    async fn execute(&mut self) -> Result<()> {
        self.start_processes()?;
        self.wait_for_services().await?;
        let document_id = self.scrape().await?;
        self.assert_artifacts(&document_id).await?;
        self.assert_grounded_query().await?;
        self.assert_model_contracts().await?;
        println!("pipeline fixture passed: {document_id}");
        Ok(())
    }

    fn start_processes(&mut self) -> Result<()> {
        for (name, binary, environment) in [
            ("scraper", "ohara-scraper", &self.scraper_environment),
            ("cleaning", "ohara-cleaning", &self.base_environment),
            ("indexer", "ohara-indexer", &self.base_environment),
            ("retrieval", "ohara-retrieval", &self.retrieval_environment),
        ] {
            self.processes.push(ManagedProcess::start(
                &self.root,
                name,
                binary,
                environment,
                &self.log_dir,
            )?);
        }
        Ok(())
    }

    async fn wait_for_services(&mut self) -> Result<()> {
        wait_for_http(
            &self.http_client,
            &format!("http://127.0.0.1:{}/health", self.scraper_port),
            Some(204),
            PROCESS_START_TIMEOUT,
        )
        .await?;
        wait_for_http(
            &self.http_client,
            &format!("http://127.0.0.1:{}/api/health", self.retrieval_port),
            None,
            PROCESS_START_TIMEOUT,
        )
        .await
    }

    async fn scrape(&self) -> Result<String> {
        let scraper_url = format!("http://127.0.0.1:{}", self.scraper_port);
        let (status, auth_payload) = request_json(
            &self.http_client,
            "POST",
            &format!("{scraper_url}/scrape"),
            Some(&json!({"topic": "deterministic fixture topic", "limit": 1})),
        )
        .await?;
        check(
            status == 401,
            format!("unauthenticated scrape returned HTTP {status}: {auth_payload}"),
        )?;
        let (status, payload) = request_json(
            &self.http_client,
            "POST",
            &format!("{}/api/topics/scrape", self.retrieval_url()),
            Some(&json!({"topic": "deterministic fixture topic", "limit": 1})),
        )
        .await?;
        check(
            status == 200,
            format!("scrape returned HTTP {status}: {payload}"),
        )?;
        check(payload.is_object(), "scrape did not return an object")?;
        check(
            payload.get("discovered") == Some(&json!(1)),
            "fixture was not discovered",
        )?;
        check(
            payload.get("enqueued") == Some(&json!(1)),
            "fixture was not enqueued",
        )?;
        let documents = payload
            .get("documents")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Message("missing fixture documents".to_owned()))?;
        check(documents.len() == 1, "missing fixture document")?;
        documents[0]
            .get("documentId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Message("fixture document has no documentId".to_owned()))
    }

    async fn assert_artifacts(&mut self, document_id: &str) -> Result<()> {
        let artifact_paths = [
            self.data_dir
                .join("raw")
                .join(format!("{document_id}.json")),
            self.data_dir
                .join("clean")
                .join(format!("{document_id}.json")),
            self.data_dir
                .join("indexed")
                .join(format!("{document_id}.json")),
        ];
        for path in &artifact_paths {
            wait_for_file(path, &mut self.processes).await?;
        }
        let clean_payload = read_json(&artifact_paths[1])?;
        let indexed_payload = read_json(&artifact_paths[2])?;
        check(
            clean_payload
                .get("markdown")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("Ohara is a private local knowledge base")),
            "clean artifact did not contain the fixture article",
        )?;
        check(
            indexed_payload
                .get("chunks")
                .and_then(Value::as_array)
                .and_then(|chunks| chunks.first())
                .and_then(|chunk| chunk.get("document_id"))
                .and_then(Value::as_str)
                == Some(document_id),
            "indexed artifact did not contain the fixture chunk",
        )?;
        self.assert_handoffs(document_id).await
    }

    async fn assert_handoffs(&mut self, document_id: &str) -> Result<()> {
        for process in ["cleaning", "indexer"] {
            let path = self
                .data_dir
                .join(format!("inbox/{process}"))
                .join(format!("{document_id}.json"));
            wait_for_absence(&path, &mut self.processes).await?;
        }
        check(
            self.data_dir
                .join("inbox/graph")
                .join(format!("{document_id}.json"))
                .is_file(),
            "graph handoff was not published",
        )
    }

    async fn assert_grounded_query(&self) -> Result<()> {
        let (status, payload) = self.query().await?;
        check(
            status == 200,
            format!("query returned HTTP {status}: {payload}"),
        )?;
        check(
            payload.get("grounding") == Some(&json!("grounded")),
            "query was not grounded",
        )?;
        check(
            payload
                .get("answer")
                .and_then(Value::as_str)
                .is_some_and(|answer| !answer.trim().is_empty()),
            "query answer was empty",
        )?;
        let chunks = payload
            .get("chunks")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Message("query returned no chunks".to_owned()))?;
        check(!chunks.is_empty(), "query returned no chunks")?;
        let citations = payload
            .get("citations")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Message("query returned no citations".to_owned()))?;
        check(!citations.is_empty(), "query returned no citations")?;
        let signals = payload
            .get("signals")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::Message("query returned no signal metadata".to_owned()))?;
        check(
            signals.get("qdrant") == Some(&json!(true)),
            "qdrant signal was unavailable",
        )?;
        check(
            signals.get("fullText") == Some(&json!(true)),
            "full-text signal was unavailable",
        )?;
        check(
            signals.get("graphPath").is_some_and(Value::is_boolean),
            "graph signal metadata was invalid",
        )?;
        check(
            citations.first() == chunks.first().and_then(|chunk| chunk.get("chunkId")),
            "citation did not reference evidence",
        )
    }

    async fn assert_model_contracts(&mut self) -> Result<()> {
        for (mode, case_name, availability) in [
            ("empty", "empty-model", "available"),
            ("unavailable", "unavailable-model", "unavailable"),
            ("malformed", "malformed-model", "unavailable"),
        ] {
            self.ollama_server.set_response_mode(mode).await?;
            let (status, payload) = self.query().await?;
            assert_contract(status, &payload, case_name, availability)?;
        }
        Ok(())
    }

    async fn query(&self) -> Result<(u16, Value)> {
        request_json(
            &self.http_client,
            "POST",
            &format!("{}/api/query", self.retrieval_url()),
            Some(&json!({"query": "What is Ohara?", "top_k": 1})),
        )
        .await
    }

    fn retrieval_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.retrieval_port)
    }

    async fn cleanup(&mut self, passed: bool) -> Result<()> {
        stop_processes(&mut self.processes).await;
        if passed {
            verify_drain_logs(&self.processes)?;
        } else {
            print_process_logs(&self.processes)?;
        }
        self.fixture_server.shutdown().await?;
        self.qdrant_server.shutdown().await?;
        self.ollama_server.shutdown().await?;
        let _ = std::fs::remove_dir_all(&self.temporary);
        Ok(())
    }
}

fn assert_contract(
    status: u16,
    payload: &Value,
    case_name: &str,
    availability: &str,
) -> Result<()> {
    check(
        status == 200,
        format!("{case_name} query returned HTTP {status}: {payload}"),
    )?;
    check(
        payload.get("availability") == Some(&json!(availability))
            && payload.get("grounding") == Some(&json!("ungrounded"))
            && payload.get("answer") == Some(&Value::Null)
            && payload.get("citations") == Some(&json!([])),
        format!("{case_name} contract was invalid: {payload}"),
    )
}

fn create_workspace() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let temporary = temporary_directory("ohara-pipeline")?;
    let data_dir = temporary.join("data");
    let log_dir = temporary.join("logs");
    std::fs::create_dir_all(&data_dir)
        .map_err(|source| Error::io("create pipeline data", source))?;
    std::fs::create_dir_all(&log_dir)
        .map_err(|source| Error::io("create pipeline logs", source))?;
    Ok((temporary, data_dir, log_dir))
}

fn pipeline_search_mode() -> Result<String> {
    let mode =
        std::env::var("OHARA_PIPELINE_QDRANT_SEARCH_MODE").unwrap_or_else(|_| "exact".to_owned());
    check(
        matches!(mode.as_str(), "exact" | "hnsw"),
        "pipeline Qdrant search mode must be exact or hnsw",
    )?;
    Ok(mode)
}

fn write_scraper_config(
    temporary: &Path,
    scraper_port: u16,
    fixture_server: &FixtureServer,
) -> Result<String> {
    let path = temporary.join("scraper-config.yaml");
    let config = format!(
        "bind: 127.0.0.1:{scraper_port}\nsearch:\n  provider: rss\n  url: http://127.0.0.1:{}/news\nfetch:\n  kind: http\n",
        fixture_server.port()
    );
    std::fs::write(&path, config)
        .map_err(|source| Error::io("write pipeline scraper config", source))?;
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Message("pipeline scraper config path is not valid UTF-8".to_owned()))
}

fn base_environment(
    data_dir: &Path,
    qdrant_port: u16,
    ollama_port: u16,
    search_mode: &str,
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
        (
            "OHARA_QDRANT_SEARCH_MODE".to_owned(),
            search_mode.to_owned(),
        ),
        (
            "OHARA_QDRANT_COLLECTION".to_owned(),
            "ohara_chunks".to_owned(),
        ),
        (
            "OHARA_PROCESS_AUTH_TOKEN".to_owned(),
            "fixture-process-token".to_owned(),
        ),
    ])
}

async fn stop_processes(processes: &mut [ManagedProcess]) {
    for process in processes.iter_mut().rev() {
        if let Err(error) = process.stop().await {
            eprintln!("{} cleanup failed: {error}", process.name);
        }
    }
}

fn verify_drain_logs(processes: &[ManagedProcess]) -> Result<()> {
    for process in processes {
        if matches!(process.name.as_str(), "cleaning" | "indexer") {
            check(
                process.log()?.contains("drain complete"),
                format!("{} did not complete its graceful drain", process.name),
            )?;
        }
    }
    Ok(())
}

async fn wait_for_file(path: &Path, processes: &mut [ManagedProcess]) -> Result<()> {
    let deadline = tokio::time::Instant::now() + PIPELINE_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if path.is_file() {
            return Ok(());
        }
        ensure_processes_running(processes)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(Error::Message(format!(
        "timed out waiting for {}",
        path.display()
    )))
}

async fn wait_for_absence(path: &Path, processes: &mut [ManagedProcess]) -> Result<()> {
    let deadline = tokio::time::Instant::now() + PIPELINE_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if !path.exists() {
            return Ok(());
        }
        ensure_processes_running(processes)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(Error::Message(format!(
        "timed out waiting for {} to be removed",
        path.display()
    )))
}

fn ensure_processes_running(processes: &mut [ManagedProcess]) -> Result<()> {
    for process in processes {
        if !process.is_running()? {
            return Err(Error::Process {
                name: process.name.clone(),
                message: format!("exited with status {:?}", process.return_code()?),
            });
        }
    }
    Ok(())
}

fn print_process_logs(processes: &[ManagedProcess]) -> Result<()> {
    for process in processes {
        let output = process.log()?;
        if !output.is_empty() {
            eprintln!("--- {} log ---\n{output}", process.name);
        }
    }
    Ok(())
}
