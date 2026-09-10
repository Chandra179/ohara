use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

use ohara::engine::{
    FetchCapabilities, FetchError, FetchPolicy, FetchValidators, Fetcher, HttpFetcher,
    HttpFetcherParams, ImpersonationFetcher, NormalizedUrl, ObscuraFetcher,
};

#[test]
fn normalized_urls_are_stable_dedup_keys() {
    let url = NormalizedUrl::parse(
        "HTTPS://Example.COM:443/article?utm_source=news&q=two&q=one#fragment",
    )
    .unwrap();
    assert_eq!(url.as_str(), "https://example.com/article?q=one&q=two");
    assert_eq!(
        NormalizedUrl::parse(url.as_str()).unwrap().as_str(),
        url.as_str()
    );
}

#[test]
fn http_fetcher_exposes_its_actual_capabilities_through_the_port() {
    let fetcher = HttpFetcher::new(HttpFetcherParams {
        user_agent: "ohara/port-test".to_string(),
        timeout: Duration::from_secs(1),
        rate_limit: Duration::ZERO,
        allow_private_hosts: true,
        max_body_bytes: 1024,
        max_redirects: 2,
    })
    .unwrap();
    let port: &dyn Fetcher = &fetcher;
    assert_eq!(
        port.capabilities(),
        FetchCapabilities {
            js_rendering: false,
            stealth: false,
        }
    );
}

#[test]
fn impersonation_fetcher_exposes_browser_profile_capabilities_through_the_port() {
    let fetcher = ImpersonationFetcher::new(
        HttpFetcherParams {
            user_agent: "ohara/port-test".to_string(),
            timeout: Duration::from_secs(1),
            rate_limit: Duration::ZERO,
            allow_private_hosts: true,
            max_body_bytes: 1024,
            max_redirects: 2,
        },
        "Mozilla/5.0 port-test".to_string(),
    )
    .unwrap();
    let port: &dyn Fetcher = &fetcher;
    assert_eq!(
        port.capabilities(),
        FetchCapabilities {
            js_rendering: false,
            stealth: true,
        }
    );
}

#[test]
fn obscura_fetcher_exposes_rendering_and_stealth_capabilities_through_the_port() {
    let fetcher = ObscuraFetcher::new(
        PathBuf::from("obscura"),
        HttpFetcherParams {
            user_agent: "ohara/port-test".to_string(),
            timeout: Duration::from_secs(1),
            rate_limit: Duration::ZERO,
            allow_private_hosts: true,
            max_body_bytes: 1024,
            max_redirects: 2,
        },
    )
    .unwrap();
    let port: &dyn Fetcher = &fetcher;
    assert_eq!(
        port.capabilities(),
        FetchCapabilities {
            js_rendering: true,
            stealth: true,
        }
    );
}

fn params(max_body_bytes: usize, allow_private_hosts: bool) -> HttpFetcherParams {
    HttpFetcherParams {
        user_agent: "ohara/port-contract".to_string(),
        timeout: Duration::from_secs(2),
        rate_limit: Duration::ZERO,
        allow_private_hosts,
        max_body_bytes,
        max_redirects: 2,
    }
}

fn plain_adapter(params: HttpFetcherParams) -> Box<dyn Fetcher> {
    Box::new(HttpFetcher::new(params).expect("valid HTTP test parameters"))
}

fn impersonated_adapter(params: HttpFetcherParams) -> Box<dyn Fetcher> {
    Box::new(
        ImpersonationFetcher::new(params, "Mozilla/5.0 port-contract".to_string())
            .expect("valid impersonation test parameters"),
    )
}

fn page(status_line: &str, headers: &str, body: &str) -> String {
    format!(
        "{status_line}\r\nContent-Type: text/html; charset=utf-8\r\n{headers}Connection: close\r\n\r\n{body}"
    )
}

fn serve_script(responses: &[String]) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    let responses = responses.to_vec();
    let handle = std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(response.as_bytes())
                .expect("write test response");
            stream.flush().expect("flush test response");
        }
    });
    (address, handle)
}

fn local_url(address: SocketAddr, path: &str) -> NormalizedUrl {
    NormalizedUrl::parse(&format!("http://{address}{path}")).expect("valid local test URL")
}

async fn assert_fetcher_policy_contract(make: fn(HttpFetcherParams) -> Box<dyn Fetcher>) {
    let (address, server) = serve_script(&[
        page("HTTP/1.1 200 OK", "ETag: v1\r\n", "<html>ok</html>"),
        "HTTP/1.1 304 Not Modified\r\nETag: v1\r\nConnection: close\r\n\r\n".to_string(),
    ]);
    let fetcher = make(params(1024, true));
    let url = local_url(address, "/document");
    let policy = FetchPolicy {
        robots: false,
        ..FetchPolicy::default()
    };
    let document = fetcher
        .fetch_with_policy(&url, &policy)
        .await
        .expect("successful fetch");
    assert_eq!(document.html, "<html>ok</html>");
    assert!(!document.js_executed);
    assert_eq!(document.final_url, url.as_str());
    assert_eq!(document.status, 200);
    assert_eq!(
        document.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(document.etag.as_deref(), Some("v1"));
    assert_eq!(document.last_modified, None);
    assert!(!document.fetched_at.is_empty());
    let not_modified = fetcher
        .fetch_with_validators(
            &url,
            &policy,
            &FetchValidators {
                etag: Some("v1".to_string()),
                last_modified: None,
            },
        )
        .await
        .expect("304 is a successful conditional result");
    assert_eq!(not_modified.status, 304);
    assert_eq!(not_modified.etag.as_deref(), Some("v1"));
    server.join().expect("test server");

    let (address, server) = serve_script(&[page("HTTP/1.1 200 OK", "", "four")]);
    let limited = make(params(3, true));
    let error = limited
        .fetch_with_policy(&local_url(address, "/large"), &policy)
        .await
        .expect_err("body limit must be enforced");
    assert!(matches!(error, FetchError::Protocol(message) if message.contains("body")));
    server.join().expect("test server");

    let (address, server) = serve_script(&[page("HTTP/1.1 404 Not Found", "", "gone")]);
    let error = make(params(1024, true))
        .fetch_with_policy(&local_url(address, "/missing"), &policy)
        .await
        .expect_err("404 must map to NotFound");
    assert!(matches!(error, FetchError::NotFound { .. }));
    assert_eq!(error.class(), ohara::Class::Permanent);
    server.join().expect("test server");

    let private = make(params(1024, false));
    let error = private
        .fetch_with_policy(
            &NormalizedUrl::parse("http://127.0.0.1:9/private").expect("private test URL"),
            &policy,
        )
        .await
        .expect_err("private targets must be refused");
    assert!(matches!(error, FetchError::Protocol(_)), "got {error:?}");
}

#[tokio::test]
async fn built_in_fetcher_adapters_share_the_fetcher_policy_contract() {
    assert_fetcher_policy_contract(plain_adapter).await;
    assert_fetcher_policy_contract(impersonated_adapter).await;
}
