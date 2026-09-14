//! Per-process peak RSS benchmark entrypoint.

#[tokio::main]
async fn main() {
    match ohara_tools::resource::run().await {
        Ok(measurements) => {
            for item in measurements {
                println!(
                    "resource benchmark: {:<9} peak RSS={:.1} MiB ({} KiB) — {}",
                    item.process,
                    item.peak_rss_mib(),
                    item.peak_rss_kib,
                    item.workload
                );
            }
            println!("resource benchmark complete: all five process workloads passed");
        }
        Err(error) => {
            eprintln!("resource benchmark failed: {error}");
            std::process::exit(1);
        }
    }
}
