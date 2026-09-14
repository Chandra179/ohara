//! Cold and warm retrieval latency benchmark entrypoint.

#[tokio::main]
async fn main() {
    if let Err(error) = ohara_tools::latency::run().await {
        eprintln!("latency benchmark failed: {error}");
        std::process::exit(1);
    }
}
