use std::time::Duration;

use ohara::engine::{
    FetchCapabilities, Fetcher, HttpFetcher, HttpFetcherParams, ImpersonationFetcher, NormalizedUrl,
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
