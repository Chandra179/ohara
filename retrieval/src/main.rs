//! Process entry point for the retrieval interface.

use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), ohara_retrieval::RetrievalError> {
    let bind = std::env::var("OHARA_RETRIEVAL_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse::<SocketAddr>()
        .map_err(|error| ohara_retrieval::RetrievalError::Configuration(error.to_string()))?;
    ohara_retrieval::run(bind).await
}
