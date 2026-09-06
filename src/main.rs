//! Binary crate root — a thin shell (README): parse args → `ohara::run()`. All
//! logic lives in the library, so everything is testable without spawning a CLI.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "ohara — embedded scrape → clean → vectorize → graph → GraphRAG pipeline

USAGE:
    ohara [--config <path.toml>]

OPTIONS:
    --config <path>    TOML config; unset knobs fall back to defaults
    -h, --help         print this help";

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut config_path: Option<PathBuf> = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--config" => {
                if let Some(path) = rest.next() {
                    config_path = Some(PathBuf::from(path));
                } else {
                    eprintln!("ohara: --config requires a path\n\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("ohara: unknown argument {other:?}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("ohara: cannot start async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(boot_and_run(config_path)) {
        Ok(()) => {
            eprintln!("ohara: shutdown complete");
            ExitCode::SUCCESS
        }
        Err(err) => {
            print_error_chain(&err);
            ExitCode::FAILURE
        }
    }
}

async fn boot_and_run(config_path: Option<PathBuf>) -> Result<(), ohara::BootError> {
    let config = ohara::config::Config::load(config_path.as_deref())?;
    ohara::run(config).await
}

/// §10: nothing swallowed silently — the full `source()` chain goes to stderr.
fn print_error_chain(err: &(dyn std::error::Error + 'static)) {
    eprintln!("ohara: {err}");
    let mut source = err.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
