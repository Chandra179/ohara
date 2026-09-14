//! Process entry point for graph extraction.

#[tokio::main]
async fn main() -> Result<(), ohara_graph::GraphError> {
    if std::env::args().nth(1).as_deref() == Some("--entity-resolution-benchmark") {
        return ohara_graph::run_entity_resolution_benchmark();
    }
    ohara_graph::run().await
}
