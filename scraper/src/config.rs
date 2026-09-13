//! Scraper process configuration loaded from `scraper/config.yaml`.

use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

use crate::ScraperError;

const CONFIG_PATH_ENV: &str = "OHARA_SCRAPER_CONFIG";
const DEFAULT_CONFIG_PATH: &str = "scraper/config.yaml";
const DEFAULT_BING_NEWS_URL: &str = "https://www.bing.com/news/search";
const DEFAULT_GOOGLE_NEWS_URL: &str = "https://news.google.com/rss/search";
const DEFAULT_BRAVE_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const DEFAULT_DUCKDUCKGO_URL: &str = "https://html.duckduckgo.com/html/";
const DEFAULT_OBSCURA_BIN: &str = "obscura";
const DEFAULT_SEARCH_COUNTRY: &str = "us";
const DEFAULT_SEARCH_LANGUAGE: &str = "en";

/// Complete configuration for one scraper process.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Address on which the scraper HTTP process listens.
    pub(crate) bind: SocketAddr,
    /// Search provider settings.
    pub(crate) search: SearchConfig,
    /// Page-fetch implementation.
    pub(crate) fetch: FetchKind,
}

/// Search provider settings.
#[derive(Clone, Debug)]
pub(crate) struct SearchConfig {
    /// Selected discovery provider.
    pub(crate) kind: SearchKind,
    /// Provider endpoint.
    pub(crate) endpoint: String,
    /// Optional Brave API credential.
    pub(crate) brave_api_key: Option<String>,
    /// Search country or region.
    pub(crate) country: String,
    /// Search language.
    pub(crate) language: String,
    /// Obscura executable used by browser-backed discovery.
    pub(crate) obscura_binary: String,
}

/// Discovery provider selected for a scraper process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchKind {
    /// Bing News RSS.
    BingNews,
    /// Google News RSS.
    GoogleNews,
    /// Brave Search JSON API.
    Brave,
    /// `DuckDuckGo` HTML rendered by Obscura.
    DuckDuckGo,
    /// A caller-provided RSS or Atom endpoint.
    Rss,
}

impl SearchKind {
    pub(crate) fn parse(value: &str) -> Result<Self, ScraperError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bing" | "bing-news" => Ok(Self::BingNews),
            "google" | "google-news" => Ok(Self::GoogleNews),
            "brave" | "brave-search" => Ok(Self::Brave),
            "ddg" | "duckduckgo" | "duck-duck-go" => Ok(Self::DuckDuckGo),
            "rss" | "atom" | "custom-rss" => Ok(Self::Rss),
            other => Err(ScraperError::Configuration(format!(
                "unsupported search.provider '{other}'; expected bing-news, google-news, brave, duckduckgo, or rss"
            ))),
        }
    }
}

/// Page fetcher selected for a scraper process.
#[derive(Clone, Debug)]
pub(crate) enum FetchKind {
    /// Fetch the original response with the process HTTP client.
    Http,
    /// Render the page with the configured Obscura binary.
    Obscura { binary: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    bind: String,
    search: FileSearchConfig,
    #[serde(default)]
    fetch: FileFetchConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSearchConfig {
    provider: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default = "default_country")]
    country: String,
    #[serde(default = "default_language")]
    language: String,
    #[serde(default)]
    brave_api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFetchConfig {
    #[serde(default = "default_fetcher")]
    kind: String,
    #[serde(default = "default_obscura_binary")]
    obscura_binary: String,
}

impl Default for FileFetchConfig {
    fn default() -> Self {
        Self {
            kind: default_fetcher(),
            obscura_binary: default_obscura_binary(),
        }
    }
}

impl Config {
    /// Loads and validates the configured scraper YAML file.
    pub(crate) fn load() -> Result<Self, ScraperError> {
        let path = env::var_os(CONFIG_PATH_ENV)
            .map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from);
        let source = fs::read_to_string(&path).map_err(|error| {
            ScraperError::Configuration(format!(
                "failed to read scraper config {}: {error}",
                path.display()
            ))
        })?;
        let expanded = expand_environment(&source)?;
        let file = serde_yaml_ng::from_str::<FileConfig>(&expanded).map_err(|error| {
            ScraperError::Configuration(format!(
                "failed to parse scraper config {}: {error}",
                path.display()
            ))
        })?;
        Self::from_file(file)
    }

    fn from_file(file: FileConfig) -> Result<Self, ScraperError> {
        let bind = file.bind.parse::<SocketAddr>().map_err(|error| {
            ScraperError::Configuration(format!("bind is not a valid socket address: {error}"))
        })?;
        let kind = SearchKind::parse(&file.search.provider)?;
        let endpoint = match non_empty(file.search.url) {
            Some(endpoint) => endpoint,
            None => default_endpoint(kind).map_or_else(
                || {
                    Err(ScraperError::Configuration(
                        "search.url is required when search.provider is rss".into(),
                    ))
                },
                |endpoint| Ok(endpoint.to_string()),
            )?,
        };
        url::Url::parse(&endpoint).map_err(|error| {
            ScraperError::Configuration(format!("search.url is not a valid URL: {error}"))
        })?;

        let brave_api_key = non_empty(file.search.brave_api_key);
        if kind == SearchKind::Brave && brave_api_key.is_none() {
            return Err(ScraperError::Configuration(
                "search.brave_api_key is required when search.provider is brave".into(),
            ));
        }

        let country = non_empty(Some(file.search.country))
            .unwrap_or_else(|| DEFAULT_SEARCH_COUNTRY.to_string());
        let language = non_empty(Some(file.search.language))
            .unwrap_or_else(|| DEFAULT_SEARCH_LANGUAGE.to_string());
        let obscura_binary = non_empty(Some(file.fetch.obscura_binary))
            .unwrap_or_else(|| DEFAULT_OBSCURA_BIN.to_string());
        let fetch = match file.fetch.kind.trim().to_ascii_lowercase().as_str() {
            "http" => FetchKind::Http,
            "obscura" => FetchKind::Obscura {
                binary: obscura_binary.clone(),
            },
            value => {
                return Err(ScraperError::Configuration(format!(
                    "unsupported fetch.kind '{value}'; expected http or obscura"
                )));
            }
        };

        Ok(Self {
            bind,
            search: SearchConfig {
                kind,
                endpoint,
                brave_api_key,
                country,
                language,
                obscura_binary,
            },
            fetch,
        })
    }
}

fn default_endpoint(kind: SearchKind) -> Option<&'static str> {
    match kind {
        SearchKind::BingNews => Some(DEFAULT_BING_NEWS_URL),
        SearchKind::GoogleNews => Some(DEFAULT_GOOGLE_NEWS_URL),
        SearchKind::Brave => Some(DEFAULT_BRAVE_URL),
        SearchKind::DuckDuckGo => Some(DEFAULT_DUCKDUCKGO_URL),
        SearchKind::Rss => None,
    }
}

fn default_country() -> String {
    DEFAULT_SEARCH_COUNTRY.to_string()
}

fn default_language() -> String {
    DEFAULT_SEARCH_LANGUAGE.to_string()
}

fn default_fetcher() -> String {
    "http".to_string()
}

fn default_obscura_binary() -> String {
    DEFAULT_OBSCURA_BIN.to_string()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn expand_environment(source: &str) -> Result<String, ScraperError> {
    let mut expanded = String::with_capacity(source.len());
    let mut remaining = source;
    while let Some(start) = remaining.find("${") {
        expanded.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = expression.find('}').ok_or_else(|| {
            ScraperError::Configuration("unterminated ${...} in scraper config".into())
        })?;
        let expression_body = &expression[..end];
        let (name, fallback) = expression_body
            .split_once(":-")
            .map_or((expression_body, None), |(name, fallback)| {
                (name, Some(fallback))
            });
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(ScraperError::Configuration(format!(
                "invalid environment variable expression '${{{expression_body}}}'"
            )));
        }
        let value = env::var(name).ok().filter(|value| !value.is_empty());
        match value.or_else(|| fallback.map(str::to_string)) {
            Some(value) => expanded.push_str(&value),
            None => {
                return Err(ScraperError::Configuration(format!(
                    "environment variable '{name}' is not set"
                )));
            }
        }
        remaining = &expression[end + 1..];
    }
    expanded.push_str(remaining);
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::{Config, FileConfig, SearchKind, expand_environment};

    #[test]
    fn parses_supported_provider_names() {
        assert!(matches!(
            SearchKind::parse("bing"),
            Ok(SearchKind::BingNews)
        ));
        assert!(matches!(
            SearchKind::parse("google-news"),
            Ok(SearchKind::GoogleNews)
        ));
        assert!(matches!(SearchKind::parse("brave"), Ok(SearchKind::Brave)));
        assert!(matches!(
            SearchKind::parse("ddg"),
            Ok(SearchKind::DuckDuckGo)
        ));
        assert!(matches!(SearchKind::parse("rss"), Ok(SearchKind::Rss)));
        assert!(SearchKind::parse("unknown").is_err());
    }

    #[test]
    fn parses_file_configuration() {
        let file = serde_yaml_ng::from_str::<FileConfig>(
            "bind: 127.0.0.1:3010\nsearch:\n  provider: google-news\n",
        )
        .ok();
        let config = file.and_then(|file| Config::from_file(file).ok());
        assert!(config.is_some());
    }

    #[test]
    fn expands_defaults_without_process_environment() {
        assert_eq!(
            expand_environment("bind: ${MISSING:-127.0.0.1:3010}").ok(),
            Some("bind: 127.0.0.1:3010".to_string())
        );
    }

    #[test]
    fn rejects_unterminated_environment_expression() {
        assert!(expand_environment("bind: ${MISSING").is_err());
    }
}
