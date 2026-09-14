//! Live Qdrant exact/HNSW benchmark entrypoint.

#[tokio::main]
async fn main() {
    match ohara_tools::qdrant::run().await {
        Ok(result) => {
            if let Err(error) = ohara_tools::qdrant::print_report(&result) {
                eprintln!("Qdrant HNSW benchmark failed: {error}");
                std::process::exit(1);
            }
            if let Some(output) = std::env::var_os("OHARA_HNSW_BENCHMARK_OUTPUT") {
                let path = std::path::PathBuf::from(output);
                match serde_json::to_vec_pretty(&result) {
                    Ok(body) => {
                        if let Err(error) = std::fs::write(&path, [body.as_slice(), b"\n"].concat())
                        {
                            eprintln!(
                                "Qdrant HNSW benchmark failed writing {}: {error}",
                                path.display()
                            );
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("Qdrant HNSW benchmark failed serializing result: {error}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Err(error) => {
            eprintln!("Qdrant HNSW benchmark failed: {error}");
            std::process::exit(1);
        }
    }
}
