//! ENGINE PLANE facade (§1.3) — the HTTP fetcher and the [`Fetcher`] port. This
//! plane owns the network; future impersonation and Obscura ladder legs must
//! remain behind the same port (§1.2.2).

mod http;
mod impersonate;
mod ladder;
mod llm;
mod obscura;
mod robots;

use std::time::Duration;

pub use http::{HttpFetcher, HttpFetcherParams};
pub use impersonate::ImpersonationFetcher;
pub use ladder::FetchLadder;
pub use llm::Ollama;
pub use obscura::ObscuraFetcher;

use async_trait::async_trait;

use crate::Class;

/// Query parameters stripped by [`NormalizedUrl::parse`] — the §8 Stage 1
/// tracking-parameter list. `utm_*` is handled as a prefix rule; these are the
/// verbatim names (sorted — [`NormalizedUrl::normalize`] binary-searches them).
const TRACKING_PARAMS: &[&str] = &[
    "_hsenc", "_hsmi", "dclid", "fbclid", "gclid", "igshid", "irclid", "mc_cid", "mc_eid",
    "msclkid", "ref_src", "ref_url", "twclid",
];

/// A validated, normalized URL — parsed once at the boundary, so invalid states
/// are unrepresentable downstream (§10 newtype discipline).
///
/// Normalization (§8 Stage 1, load-bearing for the `source_url_normalized` dedup
/// key): lowercase scheme/host and punycode IDN (the [`url`] parser does both),
/// default ports and fragments dropped, query parameters sorted, tracking
/// parameters (`utm_*` and `TRACKING_PARAMS`) stripped. Idempotent — the
/// normalized form parses to itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedUrl(url::Url);

impl NormalizedUrl {
    /// Parses `raw`, absolutizing against `final_url` when `raw` is relative
    /// (§8 Stage 1), then normalizes.
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
        Self::normalize(parsed)
    }

    /// Parses an absolute URL and normalizes it (the `ohara enqueue <url>` path:
    /// the dedup key exists before any fetch).
    ///
    /// # Errors
    /// [`UrlError::Invalid`] for unparseable input; [`UrlError::Scheme`] for
    /// anything but `http`/`https` (§12).
    pub fn parse(raw: &str) -> Result<Self, UrlError> {
        let parsed = url::Url::parse(raw).map_err(UrlError::Invalid)?;
        Self::normalize(parsed)
    }

    /// Applies the §8 Stage 1 normalization to an already-absolute URL.
    fn normalize(mut url: url::Url) -> Result<Self, UrlError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UrlError::Scheme(url.scheme().to_string()));
        }
        url.set_fragment(None);
        let mut pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.retain(|(k, _)| !is_tracking_param(k));
        pairs.sort();
        // Rebuild the query from the sorted, filtered pairs; an empty set drops
        // the `?` entirely.
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in &pairs {
            serializer.append_pair(key, value);
        }
        let rebuilt = serializer.finish();
        url.set_query(if rebuilt.is_empty() {
            None
        } else {
            Some(&rebuilt)
        });
        Ok(Self(url))
    }

    /// The normalized URL as a string — the `source_url_normalized` form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The host, for per-host policy lookups (§5 `sites`) — always lowercase
    /// (punycode for IDN).
    #[must_use]
    pub fn host_str(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    /// The underlying URL (path, port, query…) for request construction inside
    /// the engine plane.
    pub(crate) fn as_url(&self) -> &url::Url {
        &self.0
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

/// A tracking parameter per the §8 Stage 1 list: verbatim [`TRACKING_PARAMS`]
/// entries, or anything under the `utm_` prefix.
fn is_tracking_param(key: &str) -> bool {
    key.starts_with("utm_") || TRACKING_PARAMS.binary_search(&key).is_ok()
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

/// A fetch-ladder leg (§8 Stage 1). The value is engine-neutral so the pipeline
/// can translate a control-plane site hint without making the engine depend on
/// `SQLite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FetchLeg {
    /// Plain HTTP: fast and cheap, but no JavaScript or stealth support.
    Plain,
    /// Browser-profile HTTP impersonation without JavaScript execution.
    Impersonate,
    /// JavaScript-rendering and stealth subprocess.
    Browser,
}

/// Per-fetch policy handed to a [`Fetcher`] by the stage (§8 Stage 1): the stage
/// reads the control plane (`sites` overrides, config toggles) — the fetcher
/// enforces. `rate_limit` is a *floor addition*: the impl never fetches faster
/// than its own default or this value, whichever is larger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchPolicy {
    /// Minimum ladder capability to use for this request. A composite fetcher
    /// starts at this leg and may escalate when the response requires it.
    pub start_leg: FetchLeg,
    /// Minimum spacing between requests to one host (§8 politeness); `ZERO` keeps
    /// the impl's own default.
    pub rate_limit: Duration,
    /// Honor `robots.txt` for this fetch (§8: config toggle, default on).
    pub robots: bool,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            start_leg: FetchLeg::Plain,
            rate_limit: Duration::ZERO,
            robots: true,
        }
    }
}

/// HTTP validators from the last successful fetch (§7.5). A fetcher applies
/// these only to the first request; redirect targets are requested normally.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchValidators {
    /// The server's entity tag, if it supplied one.
    pub etag: Option<String>,
    /// The server's last-modified timestamp, if it supplied one.
    pub last_modified: Option<String>,
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
    /// Conditional re-crawl validators (§7.5), as served.
    pub etag: Option<String>,
    /// Conditional re-crawl validators (§7.5), as served.
    pub last_modified: Option<String>,
    /// Fetch completion timestamp (informational, RFC 3339; the §5 rule governs
    /// store columns, and the stage stamps those itself).
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
    /// The response is a JavaScript shell and requires a rendering-capable leg.
    #[error("javascript rendering required for {url}")]
    JavaScriptRequired {
        /// The URL whose response requires rendering.
        url: String,
    },
    /// The peer or subprocess violated the protocol (external behavior is data, §10).
    /// Also covers §12 SSRF refusals and unsupported content types — retried within
    /// the attempt budget, then dead.
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
            FetchError::AntiBot { .. }
            | FetchError::JavaScriptRequired { .. }
            | FetchError::Timeout { .. }
            | FetchError::Protocol(_) => Class::Retry,
            FetchError::NotFound { .. } => Class::Permanent,
        }
    }
}

impl From<UrlError> for FetchError {
    /// Inside a fetch flow, a malformed or non-HTTP URL (e.g. a redirect target)
    /// is a protocol violation, not a panic (§10: external behavior is data).
    fn from(error: UrlError) -> Self {
        FetchError::Protocol(error.to_string())
    }
}

/// One leg of the fetch ladder (§9). Contracts: rendered-or-labeled HTML; honest
/// [`capabilities`](Fetcher::capabilities); native errors collapse into
/// [`FetchError`] (contract-tested per impl); per-hop redirect re-validation and
/// the [`FetchPolicy`] floors are honored by every impl (§8, §12).
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Capability declaration — queried at composition time by the ladder.
    fn capabilities(&self) -> FetchCapabilities;

    /// Fetches `url` under `policy` (§8 politeness floor, robots toggle),
    /// following redirects — each hop re-validated per §12.
    ///
    /// # Errors
    /// [`FetchError`] per the §10 taxonomy and retry classes.
    async fn fetch_with_policy(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
    ) -> Result<FetchedDoc, FetchError>;

    /// Fetches `url` with conditional-request validators (§7.5). Implementations
    /// that do not support conditional requests may use the default behavior.
    ///
    /// # Errors
    /// [`FetchError`] per the §10 taxonomy and retry classes.
    async fn fetch_with_validators(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
        _validators: &FetchValidators,
    ) -> Result<FetchedDoc, FetchError> {
        self.fetch_with_policy(url, policy).await
    }

    /// Fetches `url` under the default policy ([`FetchPolicy::default`]).
    ///
    /// # Errors
    /// [`FetchError`] per the §10 taxonomy and retry classes.
    async fn fetch(&self, url: &NormalizedUrl) -> Result<FetchedDoc, FetchError> {
        self.fetch_with_policy(url, &FetchPolicy::default()).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use super::*;

    #[test]
    fn parse_lowercases_and_drops_fragments_and_default_ports() {
        let url = NormalizedUrl::parse("HTTP://EXAMPLE.com:80/Path/?x=1#frag").unwrap();
        assert_eq!(url.as_str(), "http://example.com/Path/?x=1");
    }

    #[test]
    fn parse_strips_tracking_params_and_sorts_the_rest() {
        let url = NormalizedUrl::parse("https://example.com/p?utm_source=feed&z=2&a=1&fbclid=abc")
            .unwrap();
        assert_eq!(url.as_str(), "https://example.com/p?a=1&z=2");
    }

    #[test]
    fn parse_drops_the_query_entirely_when_only_tracking_params_remain() {
        let url = NormalizedUrl::parse("https://example.com/p?utm_content=x").unwrap();
        assert_eq!(url.as_str(), "https://example.com/p");
    }

    #[test]
    fn parse_punycodes_internationalized_hosts() {
        let url = NormalizedUrl::parse("https://Bücher.example/lesenswert").unwrap();
        assert_eq!(url.as_str(), "https://xn--bcher-kva.example/lesenswert");
    }

    #[test]
    fn new_absolutizes_relative_forms_against_the_final_url() {
        let url = NormalizedUrl::new("/next/page?a=1", "https://example.com/dir/post").unwrap();
        assert_eq!(url.as_str(), "https://example.com/next/page?a=1");
    }

    #[test]
    fn normalization_is_idempotent() {
        let once = NormalizedUrl::parse("https://example.com/p?b=2&a=1&utm_medium=rss").unwrap();
        let twice = NormalizedUrl::parse(once.as_str()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        let err = NormalizedUrl::parse("ftp://example.com/file").unwrap_err();
        assert!(matches!(err, UrlError::Scheme(s) if s == "ftp"));
        let err = NormalizedUrl::parse("javascript:alert(1)").unwrap_err();
        assert!(matches!(err, UrlError::Scheme(_)));
    }

    #[test]
    fn unparseable_input_is_rejected() {
        assert!(matches!(
            NormalizedUrl::parse("not a url at all").unwrap_err(),
            UrlError::Invalid(_)
        ));
    }

    #[test]
    fn host_str_is_lowercase_punycode() {
        let url = NormalizedUrl::parse("https://Ünïcode.example/path").unwrap();
        let host = url.host_str();
        assert!(
            host.starts_with("xn--") && host == host.to_lowercase(),
            "IDN host must serialize as lowercase punycode, got {host}"
        );
    }

    #[test]
    fn query_values_survive_reencoding() {
        // `+` is form-encoding for a space; the rebuilt query re-encodes it.
        let url = NormalizedUrl::parse("https://example.com/q?q=rust+sqlite&lang=en").unwrap();
        assert_eq!(url.as_str(), "https://example.com/q?lang=en&q=rust+sqlite");
        let url = NormalizedUrl::parse("https://example.com/q?q=a%2Bb").unwrap();
        assert_eq!(url.as_str(), "https://example.com/q?q=a%2Bb");
        let url = NormalizedUrl::parse("https://example.com/q?weird%20key=a%26b").unwrap();
        assert_eq!(url.as_str(), "https://example.com/q?weird+key=a%26b");
    }
}
