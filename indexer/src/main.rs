//! Process entry point for the indexer.

#[tokio::main]
async fn main() -> Result<(), ohara_indexer::IndexerError> {
    ohara_indexer::run().await
}
