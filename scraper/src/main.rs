//! Process entry point for the scraper.

use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), ohara_scraper::ScraperError> {
    let bind = std::env::var("OHARA_SCRAPER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3010".to_string())
        .parse::<SocketAddr>()
        .map_err(|error| ohara_scraper::ScraperError::Configuration(error.to_string()))?;
    ohara_scraper::run(bind).await
}
