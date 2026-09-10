//! Ladder leg 3: an external Obscura JS-rendering and stealth provider (§8).
//!
//! The provider is an adapter, not a browser implementation. It starts one
//! configured executable per fetch and exchanges one JSON value over stdin and
//! stdout. Keeping this protocol here means the rest of the engine and the
//! pipeline remain independent of the browser runtime.
//!
//! Protocol version 1 uses request, success, and failure objects. Every object carries the protocol version.
//! Requests carry the normalized URL and the configured policy values `robots`, `rate_limit_ms`, `timeout_ms`, `max_body_bytes`, and `max_redirects`.
//! The request also carries `allow_private_hosts` and conditional `validators`.
//! Successful responses carry a `document` with labeled HTML, capability, final URL,
//! redirect chain, status, content type, validators, and timestamp.
//! Failures carry `error.kind`, `error.message`, and optional `error.secs`.
//! Provider error kinds are `anti_bot`, `timeout`, `not_found`,
//! `javascript_required`, and `protocol`.

//!
//! The request is sent as one JSON line. The child must emit exactly one JSON
//! response on stdout; stderr is diagnostic only. Unknown versions, malformed
//! messages, non-zero exits, unsupported statuses/content types, and false
//! `js_executed` claims become [`FetchError::Protocol`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::http::{is_fetchable_content, validate_target};
use super::robots::Robots;
use super::{
    FetchCapabilities, FetchError, FetchPolicy, FetchValidators, FetchedDoc, Fetcher, HttpFetcher,
    HttpFetcherParams, NormalizedUrl,
};

const PROTOCOL_VERSION: u32 = 1;
const AGENT_TOKEN: &str = "ohara";
const STDERR_LIMIT: usize = 4096;

#[derive(Debug, Clone)]
struct CommandSpec {
    program: PathBuf,
    args: Vec<OsString>,
}

impl CommandSpec {
    fn program(program: PathBuf) -> Self {
        Self {
            program,
            args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum RobotsOutcome {
    Rules(Robots),
    DisallowAll,
}

impl RobotsOutcome {
    fn allows(&self, path: &str) -> bool {
        match self {
            Self::Rules(robots) => robots.allows(AGENT_TOKEN, path),
            Self::DisallowAll => false,
        }
    }
}

/// JS-rendering and stealth [`Fetcher`] backed by a configured executable.
///
/// The provider keeps the same body, timeout, SSRF, robots, and politeness
/// limits as the built-in HTTP providers. The default runtime adds this leg
/// only when `fetcher.obscura_command` is configured.
pub struct ObscuraFetcher {
    command: CommandSpec,
    params: HttpFetcherParams,
    robots_fetcher: HttpFetcher,
    robots: Mutex<HashMap<String, Arc<RobotsOutcome>>>,
    last_hit: Mutex<HashMap<String, Instant>>,
}

impl std::fmt::Debug for ObscuraFetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObscuraFetcher")
            .field("command", &self.command.program)
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

impl ObscuraFetcher {
    /// Builds an Obscura provider from an executable path and shared fetch
    /// limits.
    ///
    /// The executable is not started until the first fetch, so a missing path
    /// is reported as [`FetchError::Protocol`] at fetch time rather than making
    /// configurations without Obscura impossible to inspect.
    ///
    /// # Errors
    /// [`FetchError::Protocol`] if the shared HTTP policy client cannot be built
    /// for robots and safety checks.
    pub fn new(command: PathBuf, params: HttpFetcherParams) -> Result<Self, FetchError> {
        Self::from_spec(CommandSpec::program(command), params)
    }

    fn from_spec(command: CommandSpec, params: HttpFetcherParams) -> Result<Self, FetchError> {
        let robots_fetcher = HttpFetcher::new(params.clone())?;
        Ok(Self {
            command,
            params,
            robots_fetcher,
            robots: Mutex::new(HashMap::new()),
            last_hit: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn with_args(
        program: PathBuf,
        args: Vec<String>,
        params: HttpFetcherParams,
    ) -> Result<Self, FetchError> {
        Self::from_spec(
            CommandSpec {
                program,
                args: args.into_iter().map(OsString::from).collect(),
            },
            params,
        )
    }

    async fn fetch_robots(&self, url: &NormalizedUrl, policy: &FetchPolicy) -> Arc<RobotsOutcome> {
        let key = origin_key(url);
        if let Some(outcome) = self
            .robots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return Arc::clone(outcome);
        }

        let outcome = match NormalizedUrl::parse(&format!("{key}/robots.txt")) {
            Ok(robots_url) => {
                let robots_policy = FetchPolicy {
                    start_leg: super::FetchLeg::Plain,
                    rate_limit: policy.rate_limit,
                    robots: false,
                };
                match self
                    .robots_fetcher
                    .fetch_with_policy(&robots_url, &robots_policy)
                    .await
                {
                    Ok(document) if document.status < 400 => {
                        Arc::new(RobotsOutcome::Rules(Robots::parse(&document.html)))
                    }
                    Err(FetchError::NotFound { .. }) => {
                        Arc::new(RobotsOutcome::Rules(Robots::default()))
                    }
                    _ => Arc::new(RobotsOutcome::DisallowAll),
                }
            }
            Err(_) => Arc::new(RobotsOutcome::DisallowAll),
        };
        self.robots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, Arc::clone(&outcome));
        outcome
    }

    async fn politeness_wait(&self, url: &NormalizedUrl, policy: &FetchPolicy) {
        let rate_limit = self.params.rate_limit.max(policy.rate_limit);
        let wait = {
            let mut last_hit = self
                .last_hit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let wait = last_hit.get(url.host_str()).map_or(Duration::ZERO, |last| {
                rate_limit.saturating_sub(now.duration_since(*last))
            });
            last_hit.insert(url.host_str().to_string(), now);
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    async fn invoke(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
        validators: &FetchValidators,
    ) -> Result<FetchedDoc, FetchError> {
        let request = ObscuraRequest {
            protocol_version: PROTOCOL_VERSION,
            url: url.as_str(),
            robots: policy.robots,
            rate_limit_ms: effective_rate_limit(self.params.rate_limit, policy.rate_limit),
            timeout_ms: self
                .params
                .timeout
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            max_body_bytes: self.params.max_body_bytes,
            max_redirects: self.params.max_redirects,
            allow_private_hosts: self.params.allow_private_hosts,
            validators: ObscuraValidators {
                etag: validators.etag.as_deref(),
                last_modified: validators.last_modified.as_deref(),
            },
        };
        let mut request_bytes = serde_json::to_vec(&request).map_err(|error| {
            FetchError::Protocol(format!("cannot encode Obscura request: {error}"))
        })?;
        request_bytes.push(b'\n');

        let mut command = Command::new(&self.command.program);
        command
            .args(&self.command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            FetchError::Protocol(format!(
                "cannot start Obscura executable {}: {error}",
                self.command.program.display()
            ))
        })?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            FetchError::Protocol("Obscura child did not expose stdin".to_string())
        })?;
        stdin.write_all(&request_bytes).await.map_err(|error| {
            FetchError::Protocol(format!("cannot write Obscura request: {error}"))
        })?;
        drop(stdin);

        let output = tokio::time::timeout(self.params.timeout, child.wait_with_output())
            .await
            .map_err(|_| FetchError::Timeout {
                secs: self.params.timeout.as_secs(),
            })?
            .map_err(|error| FetchError::Protocol(format!("Obscura process failed: {error}")))?;
        if !output.status.success() {
            let stderr = diagnostic(&output.stderr);
            return Err(FetchError::Protocol(format!(
                "Obscura exited with {status}: {stderr}",
                status = output.status,
            )));
        }
        decode_response(
            &output.stdout,
            url,
            self.params.max_body_bytes,
            self.params.max_redirects,
            self.params.allow_private_hosts,
        )
        .await
    }
}

#[async_trait]
impl Fetcher for ObscuraFetcher {
    fn capabilities(&self) -> FetchCapabilities {
        FetchCapabilities {
            js_rendering: true,
            stealth: true,
        }
    }

    async fn fetch_with_policy(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
    ) -> Result<FetchedDoc, FetchError> {
        self.fetch_with_validators(url, policy, &FetchValidators::default())
            .await
    }

    async fn fetch_with_validators(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
        validators: &FetchValidators,
    ) -> Result<FetchedDoc, FetchError> {
        validate_target(url, self.params.allow_private_hosts).await?;
        if policy.robots {
            let robots = self.fetch_robots(url, policy).await;
            let mut path = url.as_url().path().to_string();
            if let Some(query) = url.as_url().query() {
                path.push('?');
                path.push_str(query);
            }
            if !robots.allows(&path) {
                return Err(FetchError::Protocol(format!("robots.txt disallows {url}")));
            }
        }
        self.politeness_wait(url, policy).await;
        self.invoke(url, policy, validators).await
    }
}

#[derive(Debug, Serialize)]
struct ObscuraRequest<'a> {
    protocol_version: u32,
    url: &'a str,
    robots: bool,
    rate_limit_ms: u64,
    timeout_ms: u64,
    max_body_bytes: usize,
    max_redirects: usize,
    allow_private_hosts: bool,
    validators: ObscuraValidators<'a>,
}

#[derive(Debug, Serialize)]
struct ObscuraValidators<'a> {
    etag: Option<&'a str>,
    last_modified: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct ObscuraResponse {
    protocol_version: u32,
    #[serde(flatten)]
    result: ObscuraResult,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ObscuraResult {
    Ok { document: ObscuraDocument },
    Error { error: ObscuraError },
}

#[derive(Debug, Deserialize)]
struct ObscuraDocument {
    html: String,
    js_executed: bool,
    final_url: String,
    #[serde(default)]
    redirects: Vec<String>,
    status: u16,
    content_type: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    fetched_at: String,
}

#[derive(Debug, Deserialize)]
struct ObscuraError {
    kind: String,
    message: String,
    secs: Option<u64>,
}

async fn decode_response(
    bytes: &[u8],
    requested_url: &NormalizedUrl,
    max_body_bytes: usize,
    max_redirects: usize,
    allow_private_hosts: bool,
) -> Result<FetchedDoc, FetchError> {
    let response = serde_json::from_slice::<ObscuraResponse>(bytes)
        .map_err(|error| FetchError::Protocol(format!("invalid Obscura response: {error}")))?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(FetchError::Protocol(format!(
            "unsupported Obscura protocol version {}",
            response.protocol_version
        )));
    }
    match response.result {
        ObscuraResult::Error { error } => map_error(error, requested_url),
        ObscuraResult::Ok { document } => {
            validate_document(
                document,
                requested_url,
                max_body_bytes,
                max_redirects,
                allow_private_hosts,
            )
            .await
        }
    }
}

async fn validate_document(
    document: ObscuraDocument,
    requested_url: &NormalizedUrl,
    max_body_bytes: usize,
    max_redirects: usize,
    allow_private_hosts: bool,
) -> Result<FetchedDoc, FetchError> {
    if !document.js_executed {
        return Err(FetchError::Protocol(
            "Obscura response did not execute JavaScript".to_string(),
        ));
    }
    if document.html.len() > max_body_bytes {
        return Err(FetchError::Protocol(format!(
            "Obscura body exceeds {max_body_bytes} bytes on {requested_url}"
        )));
    }
    if !is_fetchable_content(document.content_type.as_deref()) {
        return Err(FetchError::Protocol(format!(
            "unsupported Obscura content-type {:?} on {requested_url}",
            document.content_type
        )));
    }
    if document.fetched_at.is_empty() {
        return Err(FetchError::Protocol(
            "Obscura response has an empty fetched_at".to_string(),
        ));
    }
    if document.redirects.len() > max_redirects {
        return Err(FetchError::Protocol(format!(
            "Obscura response exceeds the {max_redirects}-redirect limit on {requested_url}"
        )));
    }
    let mut base_url = requested_url.to_string();
    for redirect in &document.redirects {
        let redirect_url = NormalizedUrl::new(redirect, &base_url)?;
        validate_target(&redirect_url, allow_private_hosts).await?;
        base_url = redirect_url.to_string();
    }
    let final_url = NormalizedUrl::new(&document.final_url, &base_url)?;
    validate_target(&final_url, allow_private_hosts).await?;
    match document.status {
        304 => Ok(FetchedDoc {
            html: String::new(),
            js_executed: true,
            final_url: final_url.to_string(),
            status: document.status,
            content_type: document.content_type,
            etag: document.etag,
            last_modified: document.last_modified,
            fetched_at: document.fetched_at,
        }),
        403 | 429 => Err(FetchError::AntiBot {
            url: final_url.to_string(),
        }),
        404 | 410 => Err(FetchError::NotFound {
            url: final_url.to_string(),
        }),
        status if (200..400).contains(&status) => Ok(FetchedDoc {
            html: document.html,
            js_executed: true,
            final_url: final_url.to_string(),
            status,
            content_type: document.content_type,
            etag: document.etag,
            last_modified: document.last_modified,
            fetched_at: document.fetched_at,
        }),
        status => Err(FetchError::Protocol(format!(
            "unexpected Obscura status {status} on {final_url}"
        ))),
    }
}

fn map_error(error: ObscuraError, url: &NormalizedUrl) -> Result<FetchedDoc, FetchError> {
    let message = if error.message.is_empty() {
        "Obscura provider returned an empty error message".to_string()
    } else {
        error.message
    };
    Err(match error.kind.as_str() {
        "anti_bot" => FetchError::AntiBot {
            url: url.to_string(),
        },
        "timeout" => FetchError::Timeout {
            secs: error.secs.unwrap_or(0),
        },
        "not_found" => FetchError::NotFound {
            url: url.to_string(),
        },
        "javascript_required" => FetchError::JavaScriptRequired {
            url: url.to_string(),
        },
        "protocol" => FetchError::Protocol(message),
        other => FetchError::Protocol(format!("unknown Obscura error kind {other:?}: {message}")),
    })
}

fn origin_key(url: &NormalizedUrl) -> String {
    match url.as_url().port() {
        Some(port) => format!("{}://{}:{port}", url.as_url().scheme(), url.host_str()),
        None => format!("{}://{}", url.as_url().scheme(), url.host_str()),
    }
}

fn effective_rate_limit(default: Duration, policy: Duration) -> u64 {
    default
        .max(policy)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn diagnostic(stderr: &[u8]) -> String {
    String::from_utf8_lossy(&stderr[..stderr.len().min(STDERR_LIMIT)])
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::time::Duration;

    use super::*;

    fn params() -> HttpFetcherParams {
        HttpFetcherParams {
            user_agent: "ohara/obscura-test".to_string(),
            timeout: Duration::from_secs(2),
            rate_limit: Duration::ZERO,
            allow_private_hosts: true,
            max_body_bytes: 1024,
            max_redirects: 2,
        }
    }

    fn provider(response: &str) -> ObscuraFetcher {
        provider_with_params(response, params())
    }

    fn provider_with_params(response: &str, params: HttpFetcherParams) -> ObscuraFetcher {
        let escaped = response.replace('\'', "'\\''");
        let script = format!("read -r request; printf '%s' '{escaped}'");
        ObscuraFetcher::with_args(
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), script],
            params,
        )
        .expect("test HTTP policy is valid")
    }

    #[test]
    fn declares_rendering_and_stealth_capabilities() {
        let fetcher = ObscuraFetcher::new(PathBuf::from("obscura"), params())
            .expect("test HTTP policy is valid");
        assert_eq!(
            fetcher.capabilities(),
            FetchCapabilities {
                js_rendering: true,
                stealth: true,
            }
        );
    }

    #[tokio::test]
    async fn invokes_versioned_protocol_and_returns_labeled_document() {
        let fetcher = provider(
            r#"{"protocol_version":1,"status":"ok","document":{"html":"<main>ok</main>","js_executed":true,"final_url":"https://example.com/final","status":200,"content_type":"text/html","etag":"v1","last_modified":null,"fetched_at":"2026-09-10T00:00:00Z"}}"#,
        );
        let url = NormalizedUrl::parse("https://example.com/start").expect("valid test URL");
        let document = fetcher
            .fetch_with_policy(
                &url,
                &FetchPolicy {
                    robots: false,
                    ..FetchPolicy::default()
                },
            )
            .await
            .expect("valid protocol response");
        assert_eq!(document.html, "<main>ok</main>");
        assert!(document.js_executed);
        assert_eq!(document.final_url, "https://example.com/final");
        assert_eq!(document.etag.as_deref(), Some("v1"));
    }

    #[tokio::test]
    async fn maps_provider_errors_into_fetch_taxonomy() {
        let fetcher = provider(
            r#"{"protocol_version":1,"status":"error","error":{"kind":"anti_bot","message":"blocked","secs":null}}"#,
        );
        let url = NormalizedUrl::parse("https://example.com/start").expect("valid test URL");
        let error = fetcher
            .fetch_with_policy(
                &url,
                &FetchPolicy {
                    robots: false,
                    ..FetchPolicy::default()
                },
            )
            .await
            .expect_err("anti-bot response must be an error");
        assert!(matches!(error, FetchError::AntiBot { .. }));
        assert_eq!(error.class(), crate::Class::Retry);
    }

    #[tokio::test]
    async fn rejects_false_rendering_claims() {
        let fetcher = provider(
            r#"{"protocol_version":1,"status":"ok","document":{"html":"ok","js_executed":false,"final_url":"https://example.com","status":200,"content_type":"text/html","etag":null,"last_modified":null,"fetched_at":"now"}}"#,
        );
        let url = NormalizedUrl::parse("https://example.com").expect("valid test URL");
        let error = fetcher
            .fetch_with_policy(
                &url,
                &FetchPolicy {
                    robots: false,
                    ..FetchPolicy::default()
                },
            )
            .await
            .expect_err("false capability claim must be rejected");
        assert!(matches!(error, FetchError::Protocol(message) if message.contains("JavaScript")));
    }

    #[tokio::test]
    async fn enforces_the_configured_body_limit() {
        let mut limited = params();
        limited.max_body_bytes = 3;
        let fetcher = provider_with_params(
            r#"{"protocol_version":1,"status":"ok","document":{"html":"four","js_executed":true,"final_url":"https://example.com","status":200,"content_type":"text/html","etag":null,"last_modified":null,"fetched_at":"now"}}"#,
            limited,
        );
        let url = NormalizedUrl::parse("https://example.com").expect("valid test URL");
        let error = fetcher
            .fetch_with_policy(
                &url,
                &FetchPolicy {
                    robots: false,
                    ..FetchPolicy::default()
                },
            )
            .await
            .expect_err("oversized body must be rejected");
        assert!(matches!(error, FetchError::Protocol(message) if message.contains("body exceeds")));
    }

    #[tokio::test]
    async fn maps_http_statuses_from_the_child() {
        let fetcher = provider(
            r#"{"protocol_version":1,"status":"ok","document":{"html":"","js_executed":true,"final_url":"https://example.com/missing","status":404,"content_type":"text/html","etag":null,"last_modified":null,"fetched_at":"now"}}"#,
        );
        let url = NormalizedUrl::parse("https://example.com").expect("valid test URL");
        let error = fetcher
            .fetch_with_policy(
                &url,
                &FetchPolicy {
                    robots: false,
                    ..FetchPolicy::default()
                },
            )
            .await
            .expect_err("404 must be permanent");
        assert!(matches!(error, FetchError::NotFound { .. }));
        assert_eq!(error.class(), crate::Class::Permanent);
    }

    #[tokio::test]
    async fn rejects_a_private_final_url_when_ssrf_protection_is_on() {
        let requested_url = NormalizedUrl::parse("https://example.com").expect("valid test URL");
        let response = br#"{"protocol_version":1,"status":"ok","document":{"html":"ok","js_executed":true,"final_url":"http://127.0.0.1/private","status":200,"content_type":"text/html","etag":null,"last_modified":null,"fetched_at":"now"}}"#;
        let error = decode_response(response, &requested_url, 1024, 2, false)
            .await
            .expect_err("private final URL must be rejected");
        assert!(matches!(error, FetchError::Protocol(message) if message.contains("SSRF")));
    }

    #[tokio::test]
    async fn rejects_a_private_redirect_hop_when_ssrf_protection_is_on() {
        let requested_url = NormalizedUrl::parse("https://example.com").expect("valid test URL");
        let response = br#"{"protocol_version":1,"status":"ok","document":{"html":"ok","js_executed":true,"final_url":"https://example.com/final","redirects":["http://127.0.0.1/private"],"status":200,"content_type":"text/html","etag":null,"last_modified":null,"fetched_at":"now"}}"#;
        let error = decode_response(response, &requested_url, 1024, 2, false)
            .await
            .expect_err("private redirect must be rejected");
        assert!(matches!(error, FetchError::Protocol(message) if message.contains("SSRF")));
    }
}
