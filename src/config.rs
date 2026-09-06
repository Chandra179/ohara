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
    embedder: EmbedderConfig,
    knowledge: KnowledgeConfig,
    llm: LlmConfig,
}

/// Pinned embedder identity (§4, §11.1): model and quantization variant are one id.
#[derive(Debug, Clone)]
pub struct EmbedderConfig {
    model_id: String,
    dim: usize,
}

/// Knowledge-plane namespace pointers (§4): the read switch is atomic, the write
/// model dual-writes during a migration.
#[derive(Debug, Clone)]
pub struct KnowledgeConfig {
    read_model: String,
    write_model: String,
}

/// LLM endpoint and pinned models (§2, §11.2, §12).
#[derive(Debug, Clone)]
pub struct LlmConfig {
    base_url: url::Url,
    extraction_model: String,
    fallback_model: Option<String>,
    cloud_llm_enabled: bool,
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
    embedder: Option<RawEmbedder>,
    knowledge: Option<RawKnowledge>,
    llm: Option<RawLlm>,
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

        let config = Self {
            db_path,
            data_dir,
            poll_interval,
            lease_ttl,
            backoff_base,
            stage_events_retention,
            rate_limit,
            target_languages,
            embedder,
            knowledge,
            llm,
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
            matches!(self.llm.base_url.scheme(), "http" | "https"),
            format!(
                "llm.base_url must be http/https, got {:?}",
                self.llm.base_url.scheme()
            )
            .as_str(),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // §10: tests unwrap freely

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
}
