//! Search discovery and page-fetch provider adapters.
//!
//! Provider selection is configuration-driven so the scraper process does not
//! need to know which search index or browser is used in a deployment.

use std::process::Stdio;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::Event;
use reqwest::Client;
use serde::Deserialize;
use tokio::process::Command;

use crate::config::{Config, FetchKind, SearchConfig, SearchKind};
use crate::{MAX_DOCUMENT_BYTES, MAX_RSS_BYTES, ScraperError, TopicResult, normalize_url};

const OBSCURA_TIMEOUT: Duration = Duration::from_secs(30);
const DUCKDUCKGO_EVAL: &str = r"JSON.stringify(Array.from(document.querySelectorAll('.result')).map((result) => { const anchor = result.querySelector('.result__a'); return anchor ? { title: anchor.textContent.trim(), url: anchor.href } : null; }).filter(Boolean))";

/// Searches the configured provider and returns normalized, unique results.
pub(crate) async fn search(
    client: &Client,
    config: &Config,
    topic: &str,
    limit: usize,
) -> Result<Vec<TopicResult>, ScraperError> {
    match config.search.kind {
        SearchKind::BingNews | SearchKind::GoogleNews | SearchKind::Rss => {
            search_rss(client, &config.search, topic, limit).await
        }
        SearchKind::Brave => search_brave(client, &config.search, topic, limit).await,
        SearchKind::DuckDuckGo => search_duckduckgo(&config.search, topic, limit).await,
    }
}

/// Fetches a document through the configured page-fetching adapter.
pub(crate) async fn fetch(
    client: &Client,
    config: &Config,
    url: &str,
) -> Result<Option<Vec<u8>>, ScraperError> {
    match &config.fetch {
        FetchKind::Http => fetch_http(client, url).await,
        FetchKind::Obscura { binary } => fetch_obscura(binary, url).await,
    }
}

async fn search_rss(
    client: &Client,
    config: &SearchConfig,
    topic: &str,
    limit: usize,
) -> Result<Vec<TopicResult>, ScraperError> {
    let mut endpoint = url::Url::parse(&config.endpoint)
        .map_err(|error| ScraperError::Configuration(error.to_string()))?;
    endpoint.query_pairs_mut().append_pair("q", topic);
    match config.kind {
        SearchKind::GoogleNews => {
            endpoint
                .query_pairs_mut()
                .append_pair("hl", &config.language)
                .append_pair("gl", &config.country.to_ascii_uppercase())
                .append_pair(
                    "ceid",
                    &format!(
                        "{}:{}",
                        config.country.to_ascii_uppercase(),
                        config.language
                    ),
                );
        }
        SearchKind::BingNews | SearchKind::Rss => {
            endpoint.query_pairs_mut().append_pair("format", "rss");
        }
        SearchKind::Brave | SearchKind::DuckDuckGo => {
            return Err(ScraperError::Configuration(
                "non-RSS provider reached RSS adapter".into(),
            ));
        }
    }
    let response = client
        .get(endpoint)
        .header(
            reqwest::header::ACCEPT,
            "application/rss+xml, application/atom+xml, application/xml",
        )
        .send()
        .await
        .map_err(|error| ScraperError::Network(error.to_string()))?;
    if !response.status().is_success() {
        return Err(ScraperError::Network(format!(
            "search provider returned {}",
            response.status()
        )));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| ScraperError::Network(error.to_string()))?;
    if body.len() > MAX_RSS_BYTES {
        return Err(ScraperError::InvalidResponse(
            "RSS response is too large".into(),
        ));
    }
    parse_rss(&body, limit)
}

async fn search_brave(
    client: &Client,
    config: &SearchConfig,
    topic: &str,
    limit: usize,
) -> Result<Vec<TopicResult>, ScraperError> {
    let api_key = config.brave_api_key.as_deref().ok_or_else(|| {
        ScraperError::Configuration(
            "OHARA_SCRAPER_BRAVE_API_KEY is required when using Brave Search".into(),
        )
    })?;
    let mut endpoint = url::Url::parse(&config.endpoint)
        .map_err(|error| ScraperError::Configuration(error.to_string()))?;
    endpoint
        .query_pairs_mut()
        .append_pair("q", topic)
        .append_pair("count", &limit.to_string())
        .append_pair("country", &config.country)
        .append_pair("search_lang", &config.language);
    let response = client
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|error| ScraperError::Network(error.to_string()))?;
    if !response.status().is_success() {
        return Err(ScraperError::Network(format!(
            "Brave Search returned {}",
            response.status()
        )));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| ScraperError::Network(error.to_string()))?;
    if body.len() > MAX_RSS_BYTES {
        return Err(ScraperError::InvalidResponse(
            "Brave Search response is too large".into(),
        ));
    }
    parse_brave(&body, limit)
}

async fn search_duckduckgo(
    config: &SearchConfig,
    topic: &str,
    limit: usize,
) -> Result<Vec<TopicResult>, ScraperError> {
    let mut endpoint = url::Url::parse(&config.endpoint)
        .map_err(|error| ScraperError::Configuration(error.to_string()))?;
    endpoint.query_pairs_mut().append_pair("q", topic);
    let output = Command::new(&config.obscura_binary)
        .args([
            "fetch",
            endpoint.as_str(),
            "--eval",
            DUCKDUCKGO_EVAL,
            "--quiet",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(OBSCURA_TIMEOUT, output)
        .await
        .map_err(|_| ScraperError::Network("Obscura search timed out".into()))?
        .map_err(|error| ScraperError::Network(format!("Obscura search failed: {error}")))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(ScraperError::Network(format!(
            "Obscura search returned {}: {}",
            output.status,
            error.trim()
        )));
    }
    if output.stdout.len() > MAX_RSS_BYTES {
        return Err(ScraperError::InvalidResponse(
            "DuckDuckGo response is too large".into(),
        ));
    }
    parse_obscura_results(&output.stdout, limit)
}

fn parse_brave(body: &[u8], limit: usize) -> Result<Vec<TopicResult>, ScraperError> {
    let payload = serde_json::from_slice::<BraveResponse>(body)
        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
    Ok(normalize_results(
        payload
            .web
            .map(|web| web.results)
            .unwrap_or_default()
            .into_iter()
            .map(|result| TopicResult {
                title: result.title,
                url: result.url,
            }),
        limit,
    ))
}

fn parse_obscura_results(output: &[u8], limit: usize) -> Result<Vec<TopicResult>, ScraperError> {
    let value = serde_json::from_slice::<serde_json::Value>(output)
        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
    let value = match value {
        serde_json::Value::String(json) => serde_json::from_str(&json)
            .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?,
        value => value,
    };
    let results = serde_json::from_value::<Vec<JsonTopicResult>>(value)
        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
    Ok(normalize_results(
        results.into_iter().map(|result| TopicResult {
            title: result.title,
            url: result.url,
        }),
        limit,
    ))
}

async fn fetch_http(client: &Client, url: &str) -> Result<Option<Vec<u8>>, ScraperError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| ScraperError::Network(error.to_string()))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| ScraperError::Network(error.to_string()))?;
    if body.len() > MAX_DOCUMENT_BYTES {
        return Ok(None);
    }
    Ok(Some(body.to_vec()))
}

async fn fetch_obscura(binary: &str, url: &str) -> Result<Option<Vec<u8>>, ScraperError> {
    let output = Command::new(binary)
        .args(["fetch", url, "--dump", "html", "--quiet"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(OBSCURA_TIMEOUT, output)
        .await
        .map_err(|_| ScraperError::Network("Obscura page fetch timed out".into()))?
        .map_err(|error| ScraperError::Network(format!("Obscura page fetch failed: {error}")))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(ScraperError::Network(format!(
            "Obscura page fetch returned {}: {}",
            output.status,
            error.trim()
        )));
    }
    if output.stdout.len() > MAX_DOCUMENT_BYTES {
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

fn normalize_results(
    results: impl IntoIterator<Item = TopicResult>,
    limit: usize,
) -> Vec<TopicResult> {
    let mut normalized = Vec::with_capacity(limit);
    for result in results {
        let title = result.title.trim();
        let url = redirect_destination(&result.url);
        let Ok(url) = normalize_url(&url) else {
            continue;
        };
        if title.is_empty() || normalized.iter().any(|item: &TopicResult| item.url == url) {
            continue;
        }
        normalized.push(TopicResult {
            title: title.to_string(),
            url,
        });
        if normalized.len() >= limit {
            break;
        }
    }
    normalized
}

fn parse_rss(body: &[u8], limit: usize) -> Result<Vec<TopicResult>, ScraperError> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_item = false;
    let mut field = None;
    let mut title = String::new();
    let mut link = String::new();
    let mut results = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => match element.name().as_ref() {
                b"item" | b"entry" => {
                    in_item = true;
                    field = None;
                    title.clear();
                    link.clear();
                }
                b"title" if in_item => field = Some(Field::Title),
                b"link" if in_item => field = Some(Field::Link),
                _ => {}
            },
            Ok(Event::Text(text)) if in_item => {
                let decoded = text
                    .xml_content()
                    .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
                let value = unescape(decoded.as_ref())
                    .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
                append_field(&mut title, &mut link, field, &value);
            }
            Ok(Event::GeneralRef(reference)) if in_item => {
                let name = reference
                    .decode()
                    .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
                let value = if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?
                {
                    character.to_string()
                } else {
                    resolve_predefined_entity(name.as_ref())
                        .ok_or_else(|| {
                            ScraperError::InvalidResponse(format!("unsupported entity &{name};"))
                        })?
                        .to_string()
                };
                append_field(&mut title, &mut link, field, &value);
            }
            Ok(Event::CData(text)) if in_item => {
                append_field(
                    &mut title,
                    &mut link,
                    field,
                    &String::from_utf8_lossy(text.as_ref()),
                );
            }
            Ok(Event::Empty(element)) if in_item && element.name().as_ref() == b"link" => {
                if let Some(href) = element
                    .attributes()
                    .filter_map(Result::ok)
                    .find(|attribute| attribute.key.as_ref() == b"href")
                {
                    link = href
                        .unescape_value()
                        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?
                        .into_owned();
                }
            }
            Ok(Event::End(element)) => match element.name().as_ref() {
                b"title" | b"link" if in_item => field = None,
                b"item" | b"entry" => {
                    in_item = false;
                    let result = TopicResult {
                        title: title.clone(),
                        url: link.clone(),
                    };
                    if let Some(result) = normalize_results([result], 1).into_iter().next() {
                        results.push(result);
                        if results.len() >= limit {
                            break;
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(ScraperError::InvalidResponse(error.to_string())),
            Ok(_) => {}
        }
        buffer.clear();
    }
    Ok(results)
}

#[derive(Clone, Copy)]
enum Field {
    Title,
    Link,
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    title: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct JsonTopicResult {
    title: String,
    url: String,
}

fn append_field(title: &mut String, link: &mut String, field: Option<Field>, value: &str) {
    match field {
        Some(Field::Title) => title.push_str(value),
        Some(Field::Link) => link.push_str(value),
        None => {}
    }
}

fn redirect_destination(link: &str) -> String {
    let Ok(parsed) = url::Url::parse(link.trim()) else {
        return link.trim().to_string();
    };
    if let Some((_, value)) = parsed
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("url") || key.eq_ignore_ascii_case("uddg"))
    {
        return value.into_owned();
    }
    let lowercase = link.to_ascii_lowercase();
    let Some(marker) = lowercase.find("url%3d") else {
        return link.trim().to_string();
    };
    let mut encoded = &link[marker + 6..];
    if let Some(end) = encoded.to_ascii_lowercase().find("mkt%3d") {
        encoded = &encoded[..end];
    }
    let helper = format!("https://example.invalid/?value={encoded}");
    url::Url::parse(&helper)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "value")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_else(|| link.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{parse_brave, parse_obscura_results, parse_rss, redirect_destination};

    #[test]
    fn parses_rss_and_atom_links() {
        let body = br#"<feed><entry><title>Atom article</title><link href="https://example.com/atom" /></entry></feed>"#;
        let result = parse_rss(body, 1)
            .ok()
            .and_then(|mut results| results.pop())
            .map(|result| (result.title, result.url));
        assert_eq!(
            result,
            Some((
                "Atom article".to_string(),
                "https://example.com/atom".to_string()
            ))
        );
    }

    #[test]
    fn parses_brave_search_results() {
        let body = br#"{"web":{"results":[{"title":"Rust","url":"https://www.rust-lang.org/?utm_source=test"}]}}"#;
        let result = parse_brave(body, 1)
            .ok()
            .and_then(|mut results| results.pop())
            .map(|result| (result.title, result.url));
        assert_eq!(
            result,
            Some(("Rust".to_string(), "https://www.rust-lang.org/".to_string()))
        );
    }

    #[test]
    fn parses_obscura_json_string_output() {
        let output =
            br#""[{\"title\":\"DuckDuckGo result\",\"url\":\"https://example.com/result\"}]""#;
        let result = parse_obscura_results(output, 1)
            .ok()
            .and_then(|mut results| results.pop())
            .map(|result| (result.title, result.url));
        assert_eq!(
            result,
            Some((
                "DuckDuckGo result".to_string(),
                "https://example.com/result".to_string()
            ))
        );
    }

    #[test]
    fn unwraps_common_search_redirects() {
        assert_eq!(
            redirect_destination("https://example.com/?uddg=https%3A%2F%2Frust-lang.org"),
            "https://rust-lang.org"
        );
        assert_eq!(
            redirect_destination("https://example.com/article"),
            "https://example.com/article"
        );
    }
}
