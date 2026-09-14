//! Golden retrieval-quality benchmark entrypoint.

#[tokio::main]
async fn main() {
    if let Err(error) = ohara_tools::quality::run().await {
        eprintln!("retrieval quality failed: {error}");
        std::process::exit(1);
    }
}
