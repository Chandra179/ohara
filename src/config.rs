//! Configuration: every knob, loaded once and validated at boot (§1.2.5, §10 —
//! fail fast at startup, never mid-stage). [`Config`] is immutable after
//! construction; a model or policy change is a restart (and for models, a
//! migration, §4).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Built-in defaults — the pinned reference profile (§4, §8, §11.2).
mod defaults {
    /// Worker poll interval when the queue is empty (ms).
    pub const POLL_INTERVAL_MS: u64 = 2_000;
    /// Job lease duration (§6).
    pub const LEASE_SECS: u64 = 60;
    /// Base for jittered exponential retry backoff (§6).
    pub const BACKOFF_BASE_SECS: u64 = 60;
    /// `stage_events` retention before pruning (§5 note); 0 disables pruning.
    pub const STAGE_EVENTS_RETENTION_DAYS: u64 = 90;
    /// Global politeness default: 1 request / 2 s (§8 Stage 1).
    pub const RATE_LIMIT_MS: u64 = 2_000;
    /// Pinned embedder model (§4); the quantization variant is part of the id (§11.1).
    pub const EMBEDDER_MODEL: &str = "bge-small-en-v1.5";
    /// Pinned embedder dimensionality (§4).
    pub const EMBEDDER_DIM: usize = 384;
    /// Local Ollama endpoint (§2; nothing leaves the machine by default, §12).
    pub const LLM_BASE_URL: &str = "http://localhost:11434";
    /// Pinned extraction model (§11.2).
    pub const LLM_EXTRACTION_MODEL: &str = "phi4-mini:latest";
    /// §8 Stage 4.2: normalized-name similarity floor for same-supertype
    /// entity-resolution matches.
    pub const ER_NAME_SIMILARITY: f64 = 0.85;
    /// §8 Stage 4.2: entity-name embedding similarity floor (the `EntityNames`
    /// collection) for same-supertype matches.
    pub const ER_EMBEDDING_SIMILARITY: f64 = 0.75;
    /// §8 Stage 4: evidence chunk ids kept per fact edge.
    pub const ER_MAX_EVIDENCE: usize = 8;
    /// §8 Stage 4: `occurred_on` values kept per fact edge.
    pub const ER_MAX_OCCURRENCES: usize = 8;
    /// §8 Stage 5.2: `EntityNames` embedding-similarity floor for query
    /// entities — same conservative posture as ER's floor.
    pub const RETRIEVAL_ENTITY_THRESHOLD: f64 = 0.75;
    /// §8 Stage 5.2: cap on query entities (typed-alias hits + embedding
    /// matches) feeding the graph paths.
    pub const RETRIEVAL_MAX_QUERY_ENTITIES: usize = 8;
    /// §8 Stage 5.3: fact-graph hops from query entities (1–2 for
    /// precision-first context).
    pub const RETRIEVAL_FACT_HOPS: u8 = 2;
    /// Fetch deadline per request (§8 Stage 1).
    pub const FETCH_TIMEOUT_SECS: u64 = 30;
    /// Honest User-Agent (§8): identifies the crawler and its owner.
    pub const USER_AGENT: &str = concat!(
        "ohara/",
        env!("CARGO_PKG_VERSION"),
        " (https://github.com/Chandra179/ohara)"
    );
}

/// Boot failures from [`Config::load`] — fail fast at boot, never mid-stage (§10).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("cannot read config file {path:?}: {source}")]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The config file is not valid TOML.
    #[error("cannot parse config file: {0}")]
    Parse(#[from] toml::de::Error),
    /// A knob is invalid; the message names the knob and the expectation.
    #[error("{0}")]
    Validation(String),
}

/// Validated, immutable runtime configuration. Built only via [`Config::load`]
/// (C-BUILDER / C-STRUCT-PRIVATE); construction validates every knob.
#[derive(Debug, Clone)]
pub struct Config {
    db_path: PathBuf,
    data_dir: PathBuf,
    poll_interval: Duration,
    lease_ttl: Duration,
    backoff_base: Duration,
    stage_events_retention: Duration,
    rate_limit: Duration,
    target_languages: Vec<String>,
    pipeline: PipelineConfig,
    fetcher: FetcherConfig,
    embedder: EmbedderConfig,
    knowledge: KnowledgeConfig,
    llm: LlmConfig,
    er: ErConfig,
    retrieval: RetrievalConfig,
}

/// Pinned embedder identity (§4, §11.1): model and quantization variant are one id.
#[derive(Debug, Clone)]
pub struct EmbedderConfig {
    model_id: String,
    dim: usize,
}

/// Pipeline-level knobs (§6).
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Whether the stage chain continues past VECTORIZE into EXTRACT (§6).
    graph_enabled: bool,
}

/// Knowledge-plane namespace pointers (§4): the read switch is atomic, the write
/// model dual-writes during a migration.
#[derive(Debug, Clone)]
pub struct KnowledgeConfig {
    read_model: String,
    write_model: String,
}

/// Fetch-ladder knobs (§8 Stage 1, §12). The SSRF guard itself is not a knob:
/// only globally-routable targets are fetchable unless `allow_private_hosts` is
/// set explicitly.
#[derive(Debug, Clone)]
pub struct FetcherConfig {
    /// Honor `robots.txt` (§8; cached per host).
    robots: bool,
    /// Per-request deadline.
    timeout: Duration,
    /// Honest User-Agent header (§8).
    user_agent: String,
    /// §12 override for tests and intranets; defaults to refused.
    allow_private_hosts: bool,
}

/// LLM endpoint and pinned models (§2, §11.2, §12).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    base_url: url::Url,
    extraction_model: String,
    fallback_model: Option<String>,
    cloud_llm_enabled: bool,
}

/// Entity-resolution thresholds and fact-edge caps (§8 Stage 4). The thresholds
/// are conservative by default — validated numbers on real corpora come from the
/// eval expansion step (§15 step 8); the caps are the §8 aggregation bounds.
#[derive(Debug, Clone)]
pub struct ErConfig {
    name_sim_threshold: f64,
    embedding_sim_threshold: f64,
    max_evidence: usize,
    max_occurrences: usize,
}

/// Stage 5 retrieval knobs (§8 Stage 5): the graph path's candidate bounds and
/// the `EntityNames` similarity floor for query entities.
#[derive(Debug, Clone)]
pub struct RetrievalConfig {
    entity_embedding_threshold: f64,
    max_query_entities: usize,
    fact_hops: u8,
}

/// Raw TOML mirror — `deny_unknown_fields` so a typo'd knob fails at boot, not never.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    db_path: Option<String>,
    data_dir: Option<String>,
    poll_interval_ms: Option<u64>,
    lease_secs: Option<u64>,
    backoff_base_secs: Option<u64>,
    stage_events_retention_days: Option<u64>,
    default_rate_limit_ms: Option<u64>,
    target_languages: Option<Vec<String>>,
    pipeline: Option<RawPipeline>,
    fetcher: Option<RawFetcher>,
    embedder: Option<RawEmbedder>,
    knowledge: Option<RawKnowledge>,
    llm: Option<RawLlm>,
    er: Option<RawEr>,
    retrieval: Option<RawRetrieval>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFetcher {
    robots: Option<bool>,
    timeout_secs: Option<u64>,
    user_agent: Option<String>,
    allow_private_hosts: Option<bool>,
}

fn build_fetcher(raw: Option<&RawFetcher>) -> FetcherConfig {
    FetcherConfig {
        robots: raw.and_then(|f| f.robots).unwrap_or(true),
        timeout: Duration::from_secs(
            raw.and_then(|f| f.timeout_secs)
                .unwrap_or(defaults::FETCH_TIMEOUT_SECS),
        ),
        user_agent: raw
            .and_then(|f| f.user_agent.clone())
            .unwrap_or_else(|| defaults::USER_AGENT.to_string()),
        allow_private_hosts: raw.and_then(|f| f.allow_private_hosts).unwrap_or(false),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPipeline {
    graph_enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmbedder {
    model_id: Option<String>,
    dim: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKnowledge {
    read_model: Option<String>,
    write_model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLlm {
    base_url: Option<String>,
    extraction_model: Option<String>,
    fallback_model: Option<String>,
    cloud_llm_enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEr {
    name_similarity_threshold: Option<f64>,
    embedding_similarity_threshold: Option<f64>,
    max_evidence: Option<usize>,
    max_occurrences: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRetrieval {
    entity_embedding_threshold: Option<f64>,
    max_query_entities: Option<usize>,
    fact_hops: Option<u8>,
}

fn build_er(raw: Option<&RawEr>) -> ErConfig {
    let default_raw = RawEr::default();
    let raw = raw.unwrap_or(&default_raw);
    ErConfig {
        name_sim_threshold: raw
            .name_similarity_threshold
            .unwrap_or(defaults::ER_NAME_SIMILARITY),
        embedding_sim_threshold: raw
            .embedding_similarity_threshold
            .unwrap_or(defaults::ER_EMBEDDING_SIMILARITY),
        max_evidence: raw.max_evidence.unwrap_or(defaults::ER_MAX_EVIDENCE),
        max_occurrences: raw.max_occurrences.unwrap_or(defaults::ER_MAX_OCCURRENCES),
    }
}

fn build_retrieval(raw: Option<&RawRetrieval>) -> RetrievalConfig {
    let default_raw = RawRetrieval::default();
    let raw = raw.unwrap_or(&default_raw);
    RetrievalConfig {
        entity_embedding_threshold: raw
            .entity_embedding_threshold
            .unwrap_or(defaults::RETRIEVAL_ENTITY_THRESHOLD),
        max_query_entities: raw
            .max_query_entities
            .unwrap_or(defaults::RETRIEVAL_MAX_QUERY_ENTITIES),
        fact_hops: raw.fact_hops.unwrap_or(defaults::RETRIEVAL_FACT_HOPS),
    }
}

impl Config {
    /// Loads configuration from `path` (TOML), with built-in defaults for every
    /// unset knob; `None` means pure defaults. Pure function — no filesystem
    /// creation happens here; [`crate::run`] materializes directories.
    ///
    /// # Errors
    /// [`ConfigError::Io`] if `path` cannot be read; [`ConfigError::Parse`] on
    /// malformed TOML or unknown keys; [`ConfigError::Validation`] if any knob
    /// violates its invariant.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let raw = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p).map_err(|source| ConfigError::Io {
                    path: p.to_path_buf(),
                    source,
                })?;
                toml::from_str::<RawConfig>(&text)?
            }
            None => RawConfig::default(),
        };
        Self::build(raw)
    }

    /// Validates every knob and assembles the immutable config.
    fn build(raw: RawConfig) -> Result<Self, ConfigError> {
        let data_dir = raw
            .data_dir
            .map_or_else(|| PathBuf::from("data"), PathBuf::from);
        let db_path = raw
            .db_path
            .map_or_else(|| data_dir.join("ohara.db"), PathBuf::from);

        let poll_interval =
            Duration::from_millis(raw.poll_interval_ms.unwrap_or(defaults::POLL_INTERVAL_MS));
        let lease_ttl = Duration::from_secs(raw.lease_secs.unwrap_or(defaults::LEASE_SECS));
        let backoff_base =
            Duration::from_secs(raw.backoff_base_secs.unwrap_or(defaults::BACKOFF_BASE_SECS));
        let stage_events_retention = Duration::from_secs(
            raw.stage_events_retention_days
                .unwrap_or(defaults::STAGE_EVENTS_RETENTION_DAYS)
                * 24
                * 3600,
        );
        let rate_limit =
            Duration::from_millis(raw.default_rate_limit_ms.unwrap_or(defaults::RATE_LIMIT_MS));

        let target_languages = raw
            .target_languages
            .unwrap_or_else(|| vec!["en".to_string()]);

        let pipeline = PipelineConfig {
            graph_enabled: raw
                .pipeline
                .as_ref()
                .and_then(|p| p.graph_enabled)
                .unwrap_or(true),
        };

        let fetcher = build_fetcher(raw.fetcher.as_ref());

        let embedder = raw.embedder.as_ref();
        let embedder = EmbedderConfig {
            model_id: embedder
                .and_then(|e| e.model_id.clone())
                .unwrap_or_else(|| defaults::EMBEDDER_MODEL.to_string()),
            dim: embedder
                .and_then(|e| e.dim)
                .unwrap_or(defaults::EMBEDDER_DIM),
        };

        let knowledge = KnowledgeConfig {
            read_model: raw
                .knowledge
                .as_ref()
                .and_then(|k| k.read_model.clone())
                .unwrap_or_else(|| embedder.model_id.clone()),
            write_model: raw
                .knowledge
                .and_then(|k| k.write_model)
                .unwrap_or_else(|| embedder.model_id.clone()),
        };

        let base_url = raw.llm.as_ref().and_then(|l| l.base_url.as_deref());
        let base_url = match base_url {
            Some(s) => url::Url::parse(s).map_err(|_| {
                ConfigError::Validation(format!("llm.base_url {s:?} is not a valid URL"))
            })?,
            None => url::Url::parse(defaults::LLM_BASE_URL).map_err(|_| {
                ConfigError::Validation("built-in llm.base_url default is invalid".to_string())
            })?,
        };
        let llm = LlmConfig {
            base_url,
            extraction_model: raw
                .llm
                .as_ref()
                .and_then(|l| l.extraction_model.clone())
                .unwrap_or_else(|| defaults::LLM_EXTRACTION_MODEL.to_string()),
            fallback_model: raw.llm.as_ref().and_then(|l| l.fallback_model.clone()),
            cloud_llm_enabled: raw
                .llm
                .as_ref()
                .and_then(|l| l.cloud_llm_enabled)
                .unwrap_or(false),
        };

        let er = build_er(raw.er.as_ref());
        let retrieval = build_retrieval(raw.retrieval.as_ref());

        let config = Self {
            db_path,
            data_dir,
            poll_interval,
            lease_ttl,
            backoff_base,
            stage_events_retention,
            rate_limit,
            target_languages,
            pipeline,
            fetcher,
            embedder,
            knowledge,
            llm,
            er,
            retrieval,
        };
        config.validate()?;
        Ok(config)
    }

    /// Fails fast on any knob violating its invariant, naming the knob (§10).
    fn validate(&self) -> Result<(), ConfigError> {
        let checked = |ok: bool, message: &str| -> Result<(), ConfigError> {
            if ok {
                Ok(())
            } else {
                Err(ConfigError::Validation(message.to_string()))
            }
        };
        checked(
            !self.poll_interval.is_zero(),
            "poll_interval_ms must be > 0",
        )?;
        checked(!self.lease_ttl.is_zero(), "lease_secs must be > 0")?;
        checked(
            !self.backoff_base.is_zero(),
            "backoff_base_secs must be > 0",
        )?;
        checked(
            !self.rate_limit.is_zero(),
            "default_rate_limit_ms must be > 0",
        )?;
        checked(self.embedder.dim > 0, "embedder.dim must be > 0")?;
        checked(
            !self.embedder.model_id.is_empty(),
            "embedder.model_id must be non-empty",
        )?;
        checked(
            !self.knowledge.read_model.is_empty() && !self.knowledge.write_model.is_empty(),
            "knowledge.read_model / knowledge.write_model must be non-empty",
        )?;
        checked(
            !self.target_languages.is_empty(),
            "target_languages must not be empty (§8 Stage 2 gate)",
        )?;
        checked(
            !self.llm.extraction_model.is_empty(),
            "llm.extraction_model must be non-empty",
        )?;
        checked(
            !self.fetcher.timeout.is_zero(),
            "fetcher.timeout_secs must be > 0",
        )?;
        checked(
            !self.fetcher.user_agent.is_empty(),
            "fetcher.user_agent must be non-empty (§8: honest User-Agent)",
        )?;
        checked(
            matches!(self.llm.base_url.scheme(), "http" | "https"),
            format!(
                "llm.base_url must be http/https, got {:?}",
                self.llm.base_url.scheme()
            )
            .as_str(),
        )?;
        checked(
            self.er.name_sim_threshold > 0.0 && self.er.name_sim_threshold <= 1.0,
            "er.name_similarity_threshold must be in (0, 1]",
        )?;
        checked(
            self.er.embedding_sim_threshold > 0.0 && self.er.embedding_sim_threshold <= 1.0,
            "er.embedding_similarity_threshold must be in (0, 1]",
        )?;
        checked(
            self.er.max_evidence > 0 && self.er.max_occurrences > 0,
            "er.max_evidence / er.max_occurrences must be > 0 (§8 Stage 4 caps)",
        )?;
        checked(
            self.retrieval.entity_embedding_threshold > 0.0
                && self.retrieval.entity_embedding_threshold <= 1.0,
            "retrieval.entity_embedding_threshold must be in (0, 1]",
        )?;
        checked(
            self.retrieval.max_query_entities > 0,
            "retrieval.max_query_entities must be > 0 (§8 Stage 5.2)",
        )?;
        checked(
            self.retrieval.fact_hops > 0 && self.retrieval.fact_hops <= 2,
            "retrieval.fact_hops must be in 1..=2 (§8 Stage 5.3 precision-first)",
        )?;
        Ok(())
    }

    /// `SQLite` control-store location (default `data/ohara.db`).
    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Runtime payload root — `data/raw/`, `data/clean/` (§3).
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Worker sleep when no job is claimable.
    #[must_use]
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Job lease duration (§6).
    #[must_use]
    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    /// Base for jittered exponential retry backoff (§6).
    #[must_use]
    pub fn backoff_base(&self) -> Duration {
        self.backoff_base
    }

    /// `stage_events` retention; zero disables pruning (§5 note).
    #[must_use]
    pub fn stage_events_retention(&self) -> Duration {
        self.stage_events_retention
    }

    /// Global politeness floor between requests to one host (§8 Stage 1).
    #[must_use]
    pub fn rate_limit(&self) -> Duration {
        self.rate_limit
    }

    /// Languages accepted at the Stage 2 gate — the embedder is English-first (§8).
    #[must_use]
    pub fn target_languages(&self) -> &[String] {
        &self.target_languages
    }

    /// Pipeline-level knobs (§6).
    #[must_use]
    pub fn pipeline(&self) -> &PipelineConfig {
        &self.pipeline
    }

    /// Fetch-ladder knobs (§8 Stage 1, §12).
    #[must_use]
    pub fn fetcher(&self) -> &FetcherConfig {
        &self.fetcher
    }

    /// Pinned embedder identity (§4).
    #[must_use]
    pub fn embedder(&self) -> &EmbedderConfig {
        &self.embedder
    }

    /// Knowledge-plane namespace pointers (§4).
    #[must_use]
    pub fn knowledge(&self) -> &KnowledgeConfig {
        &self.knowledge
    }

    /// LLM endpoint and pinned models (§2, §11.2).
    #[must_use]
    pub fn llm(&self) -> &LlmConfig {
        &self.llm
    }

    /// Entity-resolution and fact-edge aggregation knobs (§8 Stage 4).
    #[must_use]
    pub fn er(&self) -> &ErConfig {
        &self.er
    }

    /// Stage 5 retrieval knobs (§8 Stage 5).
    #[must_use]
    pub fn retrieval(&self) -> &RetrievalConfig {
        &self.retrieval
    }
}

impl PipelineConfig {
    /// Whether the chain runs into EXTRACT (§6): `false` ends it at VECTORIZE —
    /// documents stay `VECTORIZED` and remain retrievable via BM25 + vector.
    #[must_use]
    pub fn graph_enabled(&self) -> bool {
        self.graph_enabled
    }
}

impl FetcherConfig {
    /// Whether `robots.txt` is honored (§8, default on).
    #[must_use]
    pub fn robots(&self) -> bool {
        self.robots
    }

    /// Per-request fetch deadline.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Honest User-Agent header (§8).
    #[must_use]
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Whether private/loopback targets are fetchable (§12 override; default no).
    #[must_use]
    pub fn allow_private_hosts(&self) -> bool {
        self.allow_private_hosts
    }
}

impl EmbedderConfig {
    /// Model id — variant (incl. quantization) is part of the identity (§11.1).
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Embedding dimensionality (384 for the pinned model, §4).
    #[must_use]
    pub fn dim(&self) -> usize {
        self.dim
    }
}

impl KnowledgeConfig {
    /// Model whose collection serves reads (the §4 atomic read switch).
    #[must_use]
    pub fn read_model(&self) -> &str {
        &self.read_model
    }

    /// Model whose collection receives writes (dual-write during migration, §4).
    #[must_use]
    pub fn write_model(&self) -> &str {
        &self.write_model
    }
}

impl LlmConfig {
    /// Local Ollama base URL (§2).
    #[must_use]
    pub fn base_url(&self) -> &url::Url {
        &self.base_url
    }

    /// Pinned extraction model (§11.2).
    #[must_use]
    pub fn extraction_model(&self) -> &str {
        &self.extraction_model
    }

    /// Optional quality-fallback model (§11.2: `llama3.1:8b-instruct-q4_K_M`).
    #[must_use]
    pub fn fallback_model(&self) -> Option<&str> {
        self.fallback_model.as_deref()
    }

    /// Cloud egress is opt-in and defaults off (§12).
    #[must_use]
    pub fn cloud_llm_enabled(&self) -> bool {
        self.cloud_llm_enabled
    }
}

impl ErConfig {
    /// Normalized-name similarity floor for same-supertype matches (§8 Stage 4.2).
    #[must_use]
    pub fn name_sim_threshold(&self) -> f64 {
        self.name_sim_threshold
    }

    /// Entity-name embedding similarity floor for same-supertype matches (§8 Stage 4.2).
    #[must_use]
    pub fn embedding_sim_threshold(&self) -> f64 {
        self.embedding_sim_threshold
    }

    /// Evidence chunk-id cap per fact edge (§8 Stage 4).
    #[must_use]
    pub fn max_evidence(&self) -> usize {
        self.max_evidence
    }

    /// `occurred_on` cap per fact edge (§8 Stage 4).
    #[must_use]
    pub fn max_occurrences(&self) -> usize {
        self.max_occurrences
    }
}

impl RetrievalConfig {
    /// `EntityNames` embedding-similarity floor for query entities (§8
    /// Stage 5.2).
    #[must_use]
    pub fn entity_embedding_threshold(&self) -> f64 {
        self.entity_embedding_threshold
    }

    /// Cap on query entities feeding the graph paths (§8 Stage 5.2).
    #[must_use]
    pub fn max_query_entities(&self) -> usize {
        self.max_query_entities
    }

    /// Fact-graph hops from query entities (§8 Stage 5.3).
    #[must_use]
    pub fn fact_hops(&self) -> u8 {
        self.fact_hops
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)] // exact-representation knob assertions

    use super::*;

    impl Config {
        /// Test-only loader from a TOML string.
        fn load_from_str(text: &str) -> Result<Self, ConfigError> {
            let raw = toml::from_str::<RawConfig>(text)?;
            Self::build(raw)
        }
    }

    #[test]
    fn defaults_load_and_validate() {
        let config = Config::load(None).expect("defaults are valid");
        assert_eq!(config.embedder().model_id(), "bge-small-en-v1.5");
        assert_eq!(config.embedder().dim(), 384);
        assert_eq!(config.knowledge().read_model(), "bge-small-en-v1.5");
        assert_eq!(config.llm().extraction_model(), "phi4-mini:latest");
        assert!(!config.llm().cloud_llm_enabled());
        assert_eq!(config.target_languages(), ["en"]);
        assert!(
            config.pipeline().graph_enabled(),
            "graph on by default (§6)"
        );
    }

    #[test]
    fn fetcher_defaults_are_honest() {
        let config = Config::load(None).expect("defaults are valid");
        assert!(config.fetcher().robots());
        assert_eq!(config.fetcher().timeout(), Duration::from_secs(30));
        assert!(config.fetcher().user_agent().starts_with("ohara/"));
        assert!(
            !config.fetcher().allow_private_hosts(),
            "§12: SSRF guard on"
        );
    }

    #[test]
    fn fetcher_overrides_apply() {
        let config = Config::load_from_str(
            "[fetcher]\nrobots = false\ntimeout_secs = 5\nallow_private_hosts = true\n",
        )
        .expect("valid config");
        assert!(!config.fetcher().robots());
        assert_eq!(config.fetcher().timeout(), Duration::from_secs(5));
        assert!(config.fetcher().allow_private_hosts());
    }

    #[test]
    fn graph_disable_stops_the_chain_knob() {
        let config =
            Config::load_from_str("[pipeline]\ngraph_enabled = false\n").expect("valid config");
        assert!(!config.pipeline().graph_enabled());
    }

    #[test]
    fn unknown_keys_fail_at_boot() {
        let err = Config::load_from_str("pol_interval_ms = 5").expect_err("typo must fail");
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn invalid_values_fail_with_named_knobs() {
        let err = Config::load_from_str("[embedder]\ndim = 0").expect_err("dim 0 must fail");
        assert!(matches!(err, ConfigError::Validation(m) if m.contains("embedder.dim")));

        let err = Config::load_from_str("[llm]\nbase_url = \"ftp://x\"").expect_err("scheme");
        assert!(matches!(err, ConfigError::Validation(m) if m.contains("http/https")));
    }

    #[test]
    fn overrides_apply() {
        let config = Config::load_from_str(
            "data_dir = \"/tmp/ohara\"\n[embedder]\nmodel_id = \"bge-small-en-v1.5:int8\"\n",
        )
        .expect("valid");
        assert_eq!(config.data_dir(), Path::new("/tmp/ohara"));
        // db_path defaults under data_dir.
        assert_eq!(config.db_path(), Path::new("/tmp/ohara/ohara.db"));
        assert_eq!(config.embedder().model_id(), "bge-small-en-v1.5:int8");
        // knowledge pointers follow the embedder override.
        assert_eq!(config.knowledge().read_model(), "bge-small-en-v1.5:int8");
    }

    #[test]
    fn er_knobs_default_and_override() {
        let config = Config::load(None).expect("defaults are valid");
        assert_eq!(config.er().name_sim_threshold(), 0.85);
        assert_eq!(config.er().embedding_sim_threshold(), 0.75);
        assert_eq!(config.er().max_evidence(), 8);
        assert_eq!(config.er().max_occurrences(), 8);

        let config = Config::load_from_str(
            "[er]\nname_similarity_threshold = 0.9\nembedding_similarity_threshold = 0.8\nmax_evidence = 3\nmax_occurrences = 2\n",
        )
        .expect("valid");
        assert_eq!(config.er().name_sim_threshold(), 0.9);
        assert_eq!(config.er().embedding_sim_threshold(), 0.8);
        assert_eq!(config.er().max_evidence(), 3);
        assert_eq!(config.er().max_occurrences(), 2);
    }

    #[test]
    fn er_thresholds_are_validated() {
        let err =
            Config::load_from_str("[er]\nname_similarity_threshold = 1.5").expect_err("must fail");
        assert!(
            matches!(err, ConfigError::Validation(m) if m.contains("name_similarity_threshold"))
        );

        let err = Config::load_from_str("[er]\nmax_evidence = 0").expect_err("must fail");
        assert!(matches!(err, ConfigError::Validation(m) if m.contains("max_evidence")));
    }

    #[test]
    fn retrieval_knobs_default_and_override() {
        let config = Config::load(None).expect("defaults are valid");
        assert_eq!(config.retrieval().entity_embedding_threshold(), 0.75);
        assert_eq!(config.retrieval().max_query_entities(), 8);
        assert_eq!(config.retrieval().fact_hops(), 2);

        let config = Config::load_from_str(
            "[retrieval]\nentity_embedding_threshold = 0.8\nmax_query_entities = 4\nfact_hops = 1\n",
        )
        .expect("valid");
        assert_eq!(config.retrieval().entity_embedding_threshold(), 0.8);
        assert_eq!(config.retrieval().max_query_entities(), 4);
        assert_eq!(config.retrieval().fact_hops(), 1);
    }

    #[test]
    fn retrieval_knobs_are_validated() {
        let err = Config::load_from_str("[retrieval]\nentity_embedding_threshold = 1.5")
            .expect_err("must fail");
        assert!(
            matches!(err, ConfigError::Validation(m) if m.contains("entity_embedding_threshold"))
        );

        let err = Config::load_from_str("[retrieval]\nfact_hops = 0").expect_err("must fail");
        assert!(matches!(err, ConfigError::Validation(m) if m.contains("fact_hops")));

        let err = Config::load_from_str("[retrieval]\nfact_hops = 3").expect_err("must fail");
        assert!(matches!(err, ConfigError::Validation(m) if m.contains("fact_hops")));

        let err =
            Config::load_from_str("[retrieval]\nmax_query_entities = 0").expect_err("must fail");
        assert!(matches!(err, ConfigError::Validation(m) if m.contains("max_query_entities")));
    }
}
