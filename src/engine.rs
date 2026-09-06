//! ENGINE PLANE facade (§1.3) — the fetch ladder (plain HTTP → impersonation →
//! Obscura) and the [`Fetcher`] port. This plane owns the network; nothing else
//! may touch it (§1.2.2).

mod http;
mod obscura;

use async_trait::async_trait;

use crate::Class;

/// A validated, absolute URL — parsed once at the boundary, so invalid states are
/// unrepresentable downstream (§10 newtype discipline). Full normalization (§8
/// Stage 1: lowercase scheme/host, punycode IDN, default-port and fragment
/// dropping, sorted query parameters, tracking-param stripping) lands with the
/// Stage 1 build step; this type is its home.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedUrl(url::Url);

impl NormalizedUrl {
    /// Parses and validates `raw`, absolutizing against `final_url` when the raw
    /// form is relative (§8 Stage 1).
    ///
    /// # Errors
    /// [`UrlError::Invalid`] for unparseable input; [`UrlError::Scheme`] for
    /// anything but `http`/`https` (§12 SSRF posture).
    pub fn new(raw: &str, final_url: &str) -> Result<Self, UrlError> {
        let base = url::Url::parse(final_url).map_err(UrlError::Invalid)?;
        let parsed = url::Url::options()
            .base_url(Some(&base))
            .parse(raw)
            .map_err(UrlError::Invalid)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(UrlError::Scheme(parsed.scheme().to_string()));
        }
        Ok(Self(parsed))
    }

    /// The absolute URL as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for NormalizedUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for NormalizedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// URL validation failures.
#[derive(Debug, thiserror::Error)]
pub enum UrlError {
    /// The input is not a parseable URL.
    #[error("invalid url: {0}")]
    Invalid(#[from] url::ParseError),
    /// The scheme is not `http`/`https` (§12).
    #[error("unsupported scheme {0:?}: only http/https are fetched")]
    Scheme(String),
}

/// What was fetched, labeled (§8 Stage 1): the contract is "return what you
/// fetched, labeled" — `js_executed` tells the pipeline whether rendering
/// happened, and the pipeline escalates when the label says it didn't.
#[derive(Debug, Clone)]
pub struct FetchedDoc {
    /// Raw response body (HTML, or whatever the server returned).
    pub html: String,
    /// Whether JavaScript rendering was executed for this fetch.
    pub js_executed: bool,
    /// URL after redirects — the base for relative-link resolution.
    pub final_url: String,
    /// HTTP status code of the final response.
    pub status: u16,
    /// Content-Type as reported by the server, if any.
    pub content_type: Option<String>,
    /// Fetch completion timestamp, §5 format.
    pub fetched_at: String,
}

/// Honest capability declaration (§9 rule 2): an impl that cannot execute JS
/// reports `js_rendering: false` — pretending otherwise would violate LSP, and the
/// ladder composes and escalates on these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchCapabilities {
    /// This fetcher can execute JavaScript.
    pub js_rendering: bool,
    /// This fetcher applies anti-bot countermeasures (impersonation, stealth).
    pub stealth: bool,
}

/// Fetch failures, unified across every ladder leg (§10). Every impl maps native
/// errors into this taxonomy; contract tests assert the retry classes.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The target served an anti-bot block.
    #[error("anti-bot block on {url}")]
    AntiBot {
        /// The blocked URL.
        url: String,
    },
    /// The fetch exceeded its deadline.
    #[error("timeout after {secs}s")]
    Timeout {
        /// The deadline, in seconds.
        secs: u64,
    },
    /// The target is gone.
    #[error("not found: {url}")]
    NotFound {
        /// The missing URL.
        url: String,
    },
    /// The peer or subprocess violated the protocol (external behavior is data, §10).
    #[error("protocol violation: {0}")]
    Protocol(String),
}

impl FetchError {
    /// Retry class per the §10 mapping: `AntiBot` and `Timeout` retry; `NotFound`
    /// is permanent; `Protocol` retries (then dies via `max_attempts` — the §10
    /// "retry, then permanent after N" shape).
    #[must_use]
    pub fn class(&self) -> Class {
        match self {
            FetchError::AntiBot { .. } | FetchError::Timeout { .. } | FetchError::Protocol(_) => {
                Class::Retry
            }
            FetchError::NotFound { .. } => Class::Permanent,
        }
    }
}

/// One leg of the fetch ladder (§9). Contracts: rendered-or-labeled HTML; honest
/// [`capabilities`](Fetcher::capabilities); native errors collapse into
/// [`FetchError`] (contract-tested per impl).
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Capability declaration — queried at composition time by the ladder.
    fn capabilities(&self) -> FetchCapabilities;

    /// Fetches `url`, following redirects (each hop re-validated, §12).
    ///
    /// # Errors
    /// [`FetchError`] per the §10 taxonomy and retry classes.
    async fn fetch(&self, url: &NormalizedUrl) -> Result<FetchedDoc, FetchError>;
}
