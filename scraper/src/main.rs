//! Process entry point for the scraper.

#[tokio::main]
async fn main() -> Result<(), ohara_scraper::ScraperError> {
    ohara_scraper::run().await
}
