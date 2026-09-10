//! Ladder leg 1 (§8 Stage 1): plain HTTP. Cannot execute JS and says so
//! ([`Fetcher::capabilities`]). Enforces the §12 SSRF posture (DNS resolved and
//! validated, pinned for the connection, every redirect hop re-validated), the
//! §8 politeness floor, and `robots.txt` — all mapped into the [`FetchError`]
//! taxonomy.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::dns::{Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode};

use super::robots::Robots;
use super::{
    FetchCapabilities, FetchError, FetchPolicy, FetchValidators, FetchedDoc, Fetcher, NormalizedUrl,
};

/// The user-agent token this crawler presents to `robots.txt` (§8: honest UA).
const AGENT_TOKEN: &str = "ohara";

/// Construction parameters — validated config knobs from `config.fetcher`.
#[derive(Debug, Clone)]
pub struct HttpFetcherParams {
    /// Honest User-Agent header (§8).
    pub user_agent: String,
    /// Per-request deadline.
    pub timeout: Duration,
    /// Default politeness floor between requests to one host (§8; the config
    /// global — `sites.rate_limit_ms` arrives per-fetch via [`FetchPolicy`]).
    pub rate_limit: Duration,
    /// §12 override: allow private/loopback/link-local targets (tests, intranets).
    pub allow_private_hosts: bool,
    /// Maximum accepted response body in bytes.
    pub max_body_bytes: usize,
    /// Maximum redirect hops, each revalidated for SSRF.
    pub max_redirects: usize,
}

/// Ladder leg 1 — the [`Fetcher`] for plain HTTP fetching. Lives as long as the
/// worker: it carries the politeness clock and the robots cache across jobs.
pub struct HttpFetcher {
    client: Client,
    timeout_secs: u64,
    rate_limit: Duration,
    max_body_bytes: usize,
    max_redirects: usize,
    capabilities: FetchCapabilities,
    robots: Mutex<HashMap<String, Arc<RobotsDecision>>>,
    last_hit: Mutex<HashMap<String, Instant>>,
}

/// The cached per-host robots verdict.
#[derive(Debug)]
struct RobotsDecision {
    robots: Robots,
    /// `true` when every path is disallowed (robots.txt was unreadable — the
    /// conservative RFC 9309 reading, cached so a flaky host is not hammered).
    disallow_all: bool,
}

impl RobotsDecision {
    fn allow_all() -> Self {
        Self {
            robots: Robots::default(),
            disallow_all: false,
        }
    }

    fn disallow_all() -> Self {
        Self {
            robots: Robots::default(),
            disallow_all: true,
        }
    }

    fn allows(&self, path: &str) -> bool {
        !self.disallow_all && self.robots.allows(AGENT_TOKEN, path)
    }
}

impl HttpFetcher {
    /// Builds the fetcher: one client with the validating resolver and redirects
    /// disabled (the ladder follows hops itself, re-validating each, §12).
    ///
    /// # Errors
    /// [`FetchError::Protocol`] if the TLS/HTTP client cannot be built — a boot
    /// problem, not a fetch problem.
    pub fn new(params: HttpFetcherParams) -> Result<Self, FetchError> {
        Self::new_with_profile(params, None)
    }

    /// Builds a browser-profile variant for the impersonation ladder leg. It
    /// shares the plain provider's DNS validation, redirects, robots, body
    /// limits, and politeness implementation; only its request profile and
    /// declared capability differ.
    pub(crate) fn new_impersonated(
        params: HttpFetcherParams,
        browser_user_agent: String,
    ) -> Result<Self, FetchError> {
        if browser_user_agent.is_empty() {
            return Err(FetchError::Protocol(
                "impersonation user-agent must be non-empty".to_string(),
            ));
        }
        let browser_user_agent = browser_user_agent.into_boxed_str();
        Self::new_with_profile(params, Some(&browser_user_agent))
    }

    fn new_with_profile(
        params: HttpFetcherParams,
        impersonation_user_agent: Option<&str>,
    ) -> Result<Self, FetchError> {
        let HttpFetcherParams {
            user_agent: plain_user_agent,
            timeout,
            rate_limit,
            allow_private_hosts,
            max_body_bytes,
            max_redirects,
        } = params;
        let capabilities = FetchCapabilities {
            js_rendering: false,
            stealth: impersonation_user_agent.is_some(),
        };
        let mut builder = Client::builder()
            .user_agent(impersonation_user_agent.unwrap_or(plain_user_agent.as_str()))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(ValidatingResolver {
                allow_private_hosts,
            }));
        let headers = if capabilities.stealth {
            browser_headers()
        } else {
            plain_headers()
        };
        builder = builder.default_headers(headers);
        let client = builder
            .build()
            .map_err(|e| FetchError::Protocol(format!("http client build failed: {e}")))?;
        Ok(Self {
            client,
            timeout_secs: timeout.as_secs(),
            rate_limit,
            max_body_bytes,
            max_redirects,
            capabilities,
            robots: Mutex::new(HashMap::new()),
            last_hit: Mutex::new(HashMap::new()),
        })
    }

    /// One request to `url` with politeness enforced — no robots, no redirects.
    async fn request_once(
        &self,
        url: &NormalizedUrl,
        validators: Option<&FetchValidators>,
        policy: &FetchPolicy,
    ) -> Result<reqwest::Response, FetchError> {
        self.politeness_wait(url, policy.rate_limit).await;
        let mut request = self.client.get(url.as_url().clone());
        if let Some(etag) = validators.and_then(|v| v.etag.as_deref()) {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = validators.and_then(|v| v.last_modified.as_deref()) {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
        }
        let response = request
            .send()
            .await
            .map_err(|e| map_request_error(&e, self.timeout_secs))?;
        Ok(response)
    }

    /// §8 politeness: never faster than the effective floor to one host. The
    /// slot is reserved *before* sleeping, so concurrent fetches serialize.
    async fn politeness_wait(&self, url: &NormalizedUrl, policy_rate_limit: Duration) {
        let rate_limit = self.rate_limit.max(policy_rate_limit);
        let wait = {
            let mut last = self
                .last_hit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let wait = last.get(url.host_str()).map_or(Duration::ZERO, |t| {
                rate_limit.saturating_sub(now.duration_since(*t))
            });
            last.insert(url.host_str().to_string(), now);
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// The per-host robots verdict, fetched and cached on first use (§8: cached
    /// per host). 404/410 means "no robots" (allow-all); any other failure is
    /// cached as disallow-all — the conservative reading of an unreadable policy.
    async fn robots_decision(
        &self,
        url: &NormalizedUrl,
        policy: &FetchPolicy,
    ) -> Result<Arc<RobotsDecision>, FetchError> {
        let key = match url.as_url().port() {
            Some(port) => format!("{}://{}:{port}", url.as_url().scheme(), url.host_str()),
            None => format!("{}://{}", url.as_url().scheme(), url.host_str()),
        };
        if let Some(hit) = self
            .robots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return Ok(Arc::clone(hit));
        }
        let robots_url = format!("{key}/robots.txt");
        let decision = match NormalizedUrl::parse(&robots_url) {
            Ok(robots_url) => match self.request_once(&robots_url, None, policy).await {
                Ok(response) => match response.status() {
                    StatusCode::NOT_FOUND | StatusCode::GONE => {
                        Arc::new(RobotsDecision::allow_all())
                    }
                    status if status.is_success() => {
                        let text = response
                            .text()
                            .await
                            .map_err(|e| FetchError::Protocol(format!("robots.txt body: {e}")))?;
                        Arc::new(RobotsDecision {
                            robots: Robots::parse(&text),
                            disallow_all: false,
                        })
                    }
                    _ => Arc::new(RobotsDecision::disallow_all()),
                },
                Err(_) => Arc::new(RobotsDecision::disallow_all()),
            },
            Err(_) => Arc::new(RobotsDecision::disallow_all()),
        };
        self.robots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, Arc::clone(&decision));
        Ok(decision)
    }

    /// Follows redirects manually (§12): each hop is a fresh [`NormalizedUrl`]
    /// (scheme re-checked) resolved through the validating resolver — the
    /// rebinding window stays closed. Returns the final response and the URL it
    /// came from (the labeled `final_url`).
    async fn follow(
        &self,
        url: &NormalizedUrl,
        validators: Option<&FetchValidators>,
        policy: &FetchPolicy,
    ) -> Result<(reqwest::Response, NormalizedUrl), FetchError> {
        let mut current = url.clone();
        let mut first_hop = validators;
        for _ in 0..=self.max_redirects {
            let response = self.request_once(&current, first_hop, policy).await?;
            if response.status() == StatusCode::NOT_MODIFIED || !response.status().is_redirection()
            {
                return Ok((response, current));
            }
            first_hop = None;
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return Err(FetchError::Protocol(format!(
                    "redirect without Location on {current}"
                )));
            };
            let location = location
                .to_str()
                .map_err(|_| FetchError::Protocol(format!("non-ASCII Location on {current}")))?;
            current = NormalizedUrl::new(location, current.as_str())?;
        }
        Err(FetchError::Protocol(format!(
            "more than {} redirects on {url}",
            self.max_redirects
        )))
    }

    /// Turns a final response into the labeled [`FetchedDoc`] (§9: return what
    /// you fetched, labeled).
    async fn finish(
        &self,
        response: reqwest::Response,
        url: &NormalizedUrl,
    ) -> Result<FetchedDoc, FetchError> {
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let last_modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if status == StatusCode::NOT_MODIFIED {
            return Ok(FetchedDoc {
                html: String::new(),
                js_executed: false,
                final_url: url.to_string(),
                status: status.as_u16(),
                content_type,
                etag,
                last_modified,
                fetched_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        if !is_fetchable_content(content_type.as_deref()) {
            return Err(FetchError::Protocol(format!(
                "unsupported content-type {:?} on {url}",
                content_type.unwrap_or_default()
            )));
        }
        match status {
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => Err(FetchError::AntiBot {
                url: url.to_string(),
            }),
            StatusCode::NOT_FOUND | StatusCode::GONE => Err(FetchError::NotFound {
                url: url.to_string(),
            }),
            status if status.is_success() => {
                let body = response
                    .bytes()
                    .await
                    .map_err(|e| map_request_error(&e, self.timeout_secs))?;
                if body.len() > self.max_body_bytes {
                    return Err(FetchError::Protocol(format!(
                        "body exceeds {} bytes on {url}",
                        self.max_body_bytes
                    )));
                }
                Ok(FetchedDoc {
                    html: String::from_utf8_lossy(&body).into_owned(),
                    js_executed: false,
                    final_url: url.to_string(),
                    status: status.as_u16(),
                    content_type,
                    etag,
                    last_modified,
                    fetched_at: chrono::Utc::now().to_rfc3339(),
                })
            }
            other => Err(FetchError::Protocol(format!(
                "unexpected status {} on {url}",
                other.as_u16()
            ))),
        }
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    fn capabilities(&self) -> FetchCapabilities {
        self.capabilities
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
        if policy.robots {
            let decision = self.robots_decision(url, policy).await?;
            let mut path_and_query = url.as_url().path().to_string();
            if let Some(query) = url.as_url().query() {
                path_and_query.push('?');
                path_and_query.push_str(query);
            }
            if !decision.allows(&path_and_query) {
                return Err(FetchError::Protocol(format!("robots.txt disallows {url}")));
            }
        }
        let (response, final_url) = self.follow(url, Some(validators), policy).await?;
        self.finish(response, &final_url).await
    }
}

fn browser_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        ),
    );
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );
    headers.insert(
        reqwest::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        reqwest::header::PRAGMA,
        HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        reqwest::header::UPGRADE_INSECURE_REQUESTS,
        HeaderValue::from_static("1"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("navigate"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static("none"),
    );
    headers
}

fn plain_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("text/html,application/xhtml+xml,*/*;q=0.8"),
    );
    headers
}

/// §12: only globally-routable addresses are fetchable. `is_global` covers
/// loopback, RFC 1918, link-local, unique-local, documentation, and shared ranges.
pub(super) fn is_fetchable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_v4(v4),
        // IPv4-mapped (::ffff:0:0/96) and deprecated IPv4-compatible addresses
        // inherit their embedded IPv4 classification.
        IpAddr::V6(v6) => v6.to_ipv4().map_or_else(|| is_global_v6(v6), is_global_v4),
    }
}

/// Resolves and validates an absolute target before handing it to a provider.
/// Built-in HTTP requests additionally pin these addresses in
/// `ValidatingResolver`; external providers receive this same boundary check.
pub(super) async fn validate_target(
    url: &NormalizedUrl,
    allow_private_hosts: bool,
) -> Result<(), FetchError> {
    if allow_private_hosts {
        return Ok(());
    }
    let host = url.host_str();
    let port = url.as_url().port_or_known_default().unwrap_or(80);
    let has_global_address = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| FetchError::Protocol(format!("cannot resolve {url}: {error}")))?
        .any(|address| is_fetchable_ip(address.ip()));
    if has_global_address {
        Ok(())
    } else {
        Err(FetchError::Protocol(format!(
            "no fetchable (global) address for {url} — §12 SSRF guard"
        )))
    }
}

fn is_global_v4(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    !(o[0] == 0
        || o[0] == 10
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        || o[0] == 127
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 172 && (16..=31).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && (o[2] == 0 || o[2] == 2))
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        || o[0] >= 224)
}

fn is_global_v6(ip: std::net::Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || s[0] == 0x0100
        || (s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0)
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x0010)
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xff00) == 0xff00)
}

/// DNS resolver that validates before connecting (§12): the resolved, validated
/// addresses are the ones hyper connects to — pinning closes the rebinding window,
/// and every redirect hop re-resolves (and re-validates) through this type.
struct ValidatingResolver {
    allow_private_hosts: bool,
}

impl Resolve for ValidatingResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private_hosts = self.allow_private_hosts;
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .filter(|addr| allow_private_hosts || is_fetchable_ip(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("no fetchable (global) address for {host} — §12 SSRF guard"),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as _)
        })
    }
}

/// Native failures collapse into the §10 taxonomy (contract-test obligation).
fn map_request_error(error: &reqwest::Error, timeout_secs: u64) -> FetchError {
    if error.is_timeout() {
        FetchError::Timeout { secs: timeout_secs }
    } else {
        FetchError::Protocol(format!("request failed: {error}"))
    }
}

/// Whether the payload is something the cleaning stage can consume. An absent
/// Content-Type is tolerated (labeled anyway); binaries are a protocol violation.
pub(super) fn is_fetchable_content(content_type: Option<&str>) -> bool {
    match content_type {
        None => true,
        Some(ct) => {
            let ct = ct
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            ct.is_empty()
                || ct.starts_with("text/")
                || ct.ends_with("html")
                || ct.ends_with("+xml")
                || ct.ends_with("json")
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // §10: tests unwrap freely

    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    const ALLOW_ALL: &str = "User-agent: *\nAllow: /\n";

    /// A scripted HTTP/1.1 server: one connection per response, in order.
    fn serve_script(responses: &[String]) -> (SocketAddr, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let responses: Vec<String> = responses.to_vec();
        let handle = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf); // request head; GET has no body
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (addr, handle)
    }

    fn page(status_line: &str, headers: &str, body: &str) -> String {
        format!(
            "{status_line}\r\nContent-Type: text/html; charset=utf-8\r\n{headers}Connection: close\r\n\r\n{body}"
        )
    }

    fn fetcher(allow_private: bool, rate: Duration) -> HttpFetcher {
        HttpFetcher::new(HttpFetcherParams {
            user_agent: "ohara/test".to_string(),
            timeout: Duration::from_secs(2),
            rate_limit: rate,
            allow_private_hosts: allow_private,
            max_body_bytes: 10 * 1024 * 1024,
            max_redirects: 5,
        })
        .unwrap()
    }

    fn url_on(addr: SocketAddr, path: &str) -> NormalizedUrl {
        NormalizedUrl::parse(&format!("http://{addr}{path}")).unwrap()
    }

    #[test]
    fn ssrf_guard_refuses_every_non_global_range() {
        use std::net::IpAddr as Ip;
        let blocked = [
            "0.0.0.1",
            "10.1.2.3",
            "100.64.0.9",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "100::1",
            "2001:2::1",
            "2001:db8::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ];
        for ip in blocked {
            let parsed: Ip = ip.parse().unwrap();
            assert!(!is_fetchable_ip(parsed), "{ip} must be refused");
        }
        let allowed = ["8.8.8.8", "93.184.216.34", "2606:4700::1111", "2620:fe::fe"];
        for ip in allowed {
            let parsed: Ip = ip.parse().unwrap();
            assert!(is_fetchable_ip(parsed), "{ip} must be fetchable");
        }
    }

    #[test]
    fn browser_profile_adds_navigation_headers() {
        let headers = browser_headers();
        assert_eq!(
            headers.get(reqwest::header::ACCEPT_LANGUAGE),
            Some(&HeaderValue::from_static("en-US,en;q=0.9"))
        );
        assert_eq!(
            headers.get("sec-fetch-mode"),
            Some(&HeaderValue::from_static("navigate"))
        );
        assert_eq!(
            headers.get(reqwest::header::UPGRADE_INSECURE_REQUESTS),
            Some(&HeaderValue::from_static("1"))
        );
    }

    #[tokio::test]
    async fn fetch_returns_a_labeled_document() {
        let (addr, server) = serve_script(&[
            page("HTTP/1.1 200 OK", "", ALLOW_ALL),
            page("HTTP/1.1 200 OK", "", "<html><body>hello</body></html>"),
        ]);
        let fetcher = fetcher(true, Duration::ZERO);
        let doc = fetcher.fetch(&url_on(addr, "/post")).await.unwrap();
        server.join().unwrap();
        assert_eq!(doc.status, 200);
        assert!(doc.html.contains("hello"));
        assert!(!doc.js_executed, "leg 1 cannot execute JS and says so (§9)");
        assert_eq!(doc.final_url, format!("http://{addr}/post"));
        assert!(
            doc.content_type
                .as_deref()
                .unwrap_or("")
                .contains("text/html")
        );
        assert!(!fetcher.capabilities().js_rendering);
    }

    #[tokio::test]
    async fn robots_disallow_is_refused() {
        let (addr, server) =
            serve_script(&[page("HTTP/1.1 200 OK", "", "User-agent: *\nDisallow: /")]);
        let fetcher = fetcher(true, Duration::ZERO);
        let err = fetcher.fetch(&url_on(addr, "/private")).await.unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, FetchError::Protocol(m) if m.contains("robots.txt disallows")));
    }

    #[tokio::test]
    async fn robots_policy_can_be_disabled_per_fetch() {
        let (addr, server) = serve_script(&[page("HTTP/1.1 200 OK", "", "<html>open</html>")]);
        let fetcher = fetcher(true, Duration::ZERO);
        let doc = fetcher
            .fetch_with_policy(
                &url_on(addr, "/anything"),
                &FetchPolicy {
                    start_leg: crate::engine::FetchLeg::Plain,
                    rate_limit: Duration::ZERO,
                    robots: false,
                },
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(doc.status, 200);
    }

    #[tokio::test]
    async fn status_mapping_matches_the_taxonomy() {
        // The robots verdict is cached per host, so only the first fetch
        // consumes a robots.txt response.
        let (addr, server) = serve_script(&[
            page("HTTP/1.1 200 OK", "", ALLOW_ALL),
            page("HTTP/1.1 404 Not Found", "", "gone"),
            page("HTTP/1.1 403 Forbidden", "", "blocked"),
        ]);
        let fetcher = fetcher(true, Duration::ZERO);
        let err = fetcher.fetch(&url_on(addr, "/gone")).await.unwrap_err();
        assert!(matches!(err, FetchError::NotFound { .. }));
        assert_eq!(err.class(), crate::Class::Permanent);
        let err = fetcher.fetch(&url_on(addr, "/blocked")).await.unwrap_err();
        assert!(matches!(err, FetchError::AntiBot { .. }));
        assert_eq!(err.class(), crate::Class::Retry);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn redirects_are_followed_and_labeled_with_the_final_url() {
        let (addr, server) = serve_script(&[
            page("HTTP/1.1 200 OK", "", ALLOW_ALL),
            page(
                "HTTP/1.1 301 Moved Permanently",
                "Location: /final?a=1\r\n",
                "",
            ),
            page("HTTP/1.1 200 OK", "", "<html>moved</html>"),
        ]);
        let fetcher = fetcher(true, Duration::ZERO);
        let doc = fetcher.fetch(&url_on(addr, "/old")).await.unwrap();
        server.join().unwrap();
        assert_eq!(doc.status, 200);
        assert_eq!(doc.final_url, format!("http://{addr}/final?a=1"));
        assert!(doc.html.contains("moved"));
    }

    #[tokio::test]
    async fn private_targets_are_blocked_by_default() {
        let fetcher = fetcher(false, Duration::ZERO);
        // 127.0.0.1 is loopback: the §12 guard refuses it before any request.
        let url = NormalizedUrl::parse("http://127.0.0.1:9/x").unwrap();
        let err = fetcher.fetch(&url).await.unwrap_err();
        assert!(matches!(err, FetchError::Protocol(_)));
        assert_eq!(err.class(), crate::Class::Retry);
    }

    #[tokio::test]
    async fn unreadable_content_types_are_protocol_violations() {
        let (addr, server) = serve_script(&[
            page("HTTP/1.1 200 OK", "", ALLOW_ALL),
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nConnection: close\r\n\r\n%PDF-1.4"
                .to_string(),
        ]);
        let fetcher = fetcher(true, Duration::ZERO);
        let err = fetcher.fetch(&url_on(addr, "/doc.pdf")).await.unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, FetchError::Protocol(m) if m.contains("content-type")));
    }

    #[tokio::test]
    async fn politeness_floor_serializes_requests_to_one_host() {
        let (addr, _server) = serve_script(&[
            page("HTTP/1.1 200 OK", "", ALLOW_ALL),
            page("HTTP/1.1 200 OK", "", "one"),
            page("HTTP/1.1 200 OK", "", "two"),
        ]);
        let fetcher = fetcher(true, Duration::from_millis(250));
        let started = Instant::now();
        fetcher.fetch(&url_on(addr, "/one")).await.unwrap();
        fetcher.fetch(&url_on(addr, "/two")).await.unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(240),
            "second request must wait for the §8 politeness floor"
        );
    }

    #[tokio::test]
    async fn per_fetch_rate_limit_is_at_least_the_global_floor() {
        let (addr, _server) = serve_script(&[
            page("HTTP/1.1 200 OK", "", "one"),
            page("HTTP/1.1 200 OK", "", "two"),
        ]);
        let fetcher = fetcher(true, Duration::ZERO);
        let policy = FetchPolicy {
            start_leg: crate::engine::FetchLeg::Plain,
            rate_limit: Duration::from_millis(250),
            robots: false,
        };
        let started = Instant::now();
        fetcher
            .fetch_with_policy(&url_on(addr, "/one"), &policy)
            .await
            .unwrap();
        fetcher
            .fetch_with_policy(&url_on(addr, "/two"), &policy)
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(240),
            "site policy must raise the effective per-host floor"
        );
    }

    #[tokio::test]
    async fn request_timeout_maps_to_the_timeout_variant() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            // Never respond: the client must hit its deadline.
            std::thread::sleep(Duration::from_secs(2));
        });
        let fetcher = HttpFetcher::new(HttpFetcherParams {
            user_agent: "ohara/test".to_string(),
            timeout: Duration::from_millis(300),
            rate_limit: Duration::ZERO,
            allow_private_hosts: true,
            max_body_bytes: 10 * 1024 * 1024,
            max_redirects: 5,
        })
        .unwrap();
        let err = fetcher
            .fetch_with_policy(
                &url_on(addr, "/slow"),
                &FetchPolicy {
                    start_leg: crate::engine::FetchLeg::Plain,
                    rate_limit: Duration::ZERO,
                    robots: false,
                },
            )
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, FetchError::Timeout { .. }), "got {err:?}");
    }
}
