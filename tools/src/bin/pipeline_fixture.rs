//! Deterministic process-boundary fixture entrypoint.

#[tokio::main]
async fn main() {
    if let Err(error) = ohara_tools::pipeline_fixture::run().await {
        eprintln!("pipeline fixture failed: {error}");
        std::process::exit(1);
    }
}
