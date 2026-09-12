//! Topic discovery over the network.
//!
//! Search is an Engine concern because this module owns the outbound HTTP
//! connection and the provider-specific RSS response. The server and control
//! planes receive only normalized, provider-neutral results.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use quick_xml::Reader;
use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::Event;
use reqwest::Client;

use super::NormalizedUrl;

const BING_NEWS_ENDPOINT: &str = "https://www.bing.com/news/search";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// One result returned by a topic search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicResult {
    /// The title supplied by the search provider.
    pub title: String,
    /// The validated and normalized article URL.
    pub url: NormalizedUrl,
}

/// Network failures and malformed provider responses from topic search.
#[derive(Debug, thiserror::Error)]
pub enum TopicSearchError {
    /// The search provider could not be reached or returned a non-success status.
    #[error("topic search unavailable: {0}")]
    Unavailable(String),
    /// The provider response could not be decoded into usable search results.
    #[error("topic search returned an invalid response: {0}")]
    InvalidResponse(String),
}

/// Port for discovering URLs for a human-entered topic.
#[async_trait]
pub trait TopicSearcher: Send + Sync {
    /// Searches for up to `limit` article results.
    async fn search(&self, topic: &str, limit: usize)
    -> Result<Vec<TopicResult>, TopicSearchError>;
}

/// Bing News RSS adapter used by the local topic workflow.
pub struct BingNewsSearcher {
    client: Client,
    user_agent: String,
    timeout: Duration,
}

impl BingNewsSearcher {
    /// Builds the adapter without performing network I/O.
    #[must_use]
    pub fn new(user_agent: String, timeout: Duration) -> Self {
        Self {
            client: Client::new(),
            user_agent,
            timeout,
        }
    }

    async fn request(&self, topic: &str) -> Result<Vec<u8>, TopicSearchError> {
        let mut endpoint = url::Url::parse(BING_NEWS_ENDPOINT).map_err(|error| {
            TopicSearchError::InvalidResponse(format!("invalid search endpoint: {error}"))
        })?;
        endpoint
            .query_pairs_mut()
            .append_pair("q", topic)
            .append_pair("format", "rss");
        let response = self
            .client
            .get(endpoint)
            .header(
                reqwest::header::ACCEPT,
                "application/rss+xml, application/xml",
            )
            .header(reqwest::header::USER_AGENT, &self.user_agent)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| TopicSearchError::Unavailable(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(TopicSearchError::Unavailable(format!(
                "search provider returned HTTP {status}"
            )));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| TopicSearchError::Unavailable(error.to_string()))?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(TopicSearchError::InvalidResponse(format!(
                "response exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        Ok(body.to_vec())
    }
}

#[async_trait]
impl TopicSearcher for BingNewsSearcher {
    async fn search(
        &self,
        topic: &str,
        limit: usize,
    ) -> Result<Vec<TopicResult>, TopicSearchError> {
        let body = self.request(topic).await?;
        parse_rss(&body, limit)
    }
}

#[derive(Clone, Copy)]
enum ItemField {
    Title,
    Link,
}

fn parse_rss(body: &[u8], limit: usize) -> Result<Vec<TopicResult>, TopicSearchError> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_item = false;
    let mut field = None;
    let mut title = String::new();
    let mut link = String::new();
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => match element.name().as_ref() {
                b"item" => {
                    in_item = true;
                    field = None;
                    title.clear();
                    link.clear();
                }
                b"title" if in_item => field = Some(ItemField::Title),
                b"link" if in_item => field = Some(ItemField::Link),
                _ => {}
            },
            Ok(Event::Text(text)) if in_item => {
                let decoded = text
                    .xml_content()
                    .map_err(|error| TopicSearchError::InvalidResponse(error.to_string()))?;
                let value = unescape(decoded.as_ref())
                    .map_err(|error| TopicSearchError::InvalidResponse(error.to_string()))?;
                append_value(&mut title, &mut link, field, &value);
            }
            Ok(Event::GeneralRef(reference)) if in_item => {
                let name = reference
                    .decode()
                    .map_err(|error| TopicSearchError::InvalidResponse(error.to_string()))?;
                let value = if let Some(character) = reference
                    .resolve_char_ref()
                    .map_err(|error| TopicSearchError::InvalidResponse(error.to_string()))?
                {
                    character.to_string()
                } else {
                    resolve_predefined_entity(name.as_ref())
                        .ok_or_else(|| {
                            TopicSearchError::InvalidResponse(format!(
                                "unsupported XML entity &{name};"
                            ))
                        })?
                        .to_string()
                };
                append_value(&mut title, &mut link, field, &value);
            }
            Ok(Event::CData(text)) if in_item => {
                let value = String::from_utf8_lossy(text.as_ref());
                append_value(&mut title, &mut link, field, &value);
            }
            Ok(Event::End(element)) => match element.name().as_ref() {
                b"title" | b"link" if in_item => field = None,
                b"item" => {
                    in_item = false;
                    if let Some(result) = build_result(&title, &link)
                        && seen.insert(result.url.as_str().to_string())
                    {
                        results.push(result);
                        if results.len() >= limit {
                            break;
                        }
                    }
                    field = None;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(TopicSearchError::InvalidResponse(error.to_string()));
            }
            Ok(_) => {}
        }
        buffer.clear();
    }
    Ok(results)
}

fn append_value(title: &mut String, link: &mut String, field: Option<ItemField>, value: &str) {
    match field {
        Some(ItemField::Title) => title.push_str(value),
        Some(ItemField::Link) => link.push_str(value),
        None => {}
    }
}

fn build_result(title: &str, link: &str) -> Option<TopicResult> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let destination = bing_destination(link.trim()).unwrap_or_else(|| link.trim().to_string());
    let url = NormalizedUrl::parse(&destination).ok()?;
    Some(TopicResult {
        title: title.to_string(),
        url,
    })
}

fn bing_destination(link: &str) -> Option<String> {
    let parsed = url::Url::parse(link).ok()?;
    if let Some((_, value)) = parsed
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("url"))
    {
        return Some(value.into_owned());
    }

    // Some Bing RSS responses collapse the redirect query into one encoded
    // value (`...url%3Dhttps%3A...`). Recover that form without following the
    // redirect, so the queue stores the article URL rather than a Bing link.
    let lowercase = link.to_ascii_lowercase();
    let marker = lowercase.find("url%3d")?;
    let mut encoded = &link[marker + "url%3d".len()..];
    if let Some(end) = encoded.to_ascii_lowercase().find("mkt%3d") {
        encoded = &encoded[..end];
    }
    let helper = format!("https://example.invalid/?value={encoded}");
    let decoded = url::Url::parse(&helper)
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == "value")
        .map(|(_, value)| value.into_owned())?;
    (!decoded.is_empty()).then_some(decoded)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::{build_result, parse_rss};

    #[test]
    fn parses_and_normalizes_news_results() {
        let body = br#"<?xml version="1.0"?><rss><channel><item><title>Example story</title><link>https://www.bing.com/news/apiclick.aspx?ref=FexRss&amp;url=https%3A%2F%2FEXAMPLE.com%2Fstory%3Futm_source%3Dbing</link></item></channel></rss>"#;
        let results = parse_rss(body, 5).unwrap();
        assert_eq!(results[0].title, "Example story");
        assert_eq!(results[0].url.as_str(), "https://example.com/story");
    }

    #[test]
    fn rejects_non_http_result_urls() {
        assert!(build_result("Example", "javascript:alert(1)").is_none());
    }

    #[test]
    fn recovers_bings_collapsed_encoded_redirect() {
        let link = "http://www.bing.com/news/apiclick.aspx?ref=FexRssaid%3Dtid%3D1url%3Dhttps%3A%2F%2Fexample.com%2Fstory%3Fmkt%3Den-id";
        let result = build_result("Example", link).unwrap();
        assert_eq!(result.url.as_str(), "https://example.com/story");
    }

    #[test]
    fn applies_the_result_limit_and_deduplicates_urls() {
        let body = br"<rss><channel><item><title>One</title><link>https://example.com/a</link></item><item><title>Same</title><link>https://example.com/a#fragment</link></item><item><title>Two</title><link>https://example.com/b</link></item></channel></rss>";
        let results = parse_rss(body, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "One");
    }
}
