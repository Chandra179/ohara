//! Process entry point for graph extraction.

#[tokio::main]
async fn main() -> Result<(), ohara_graph::GraphError> {
    ohara_graph::run().await
}
