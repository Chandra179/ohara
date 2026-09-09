//! Binary crate root — a thin shell (README): parse args, then call the library
//! worker or operator query entry point. All domain logic remains testable without
//! spawning a CLI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "ohara — embedded scrape → clean → vectorize → graph → GraphRAG pipeline

USAGE:
    ohara [--config <path.toml>]
    ohara query <text> [--config <path.toml>] [--top-k <n>]
    ohara backup <directory> [--config <path.toml>]

OPTIONS:
    --config <path>    TOML config; unset knobs fall back to defaults
    --top-k <n>        Number of ranked chunks to print for query (config default)
    query <text>       Retrieve ranked chunks with chunk-id citations
    backup <directory> Write a consistent, non-overwriting snapshot
    -h, --help         print this help";

#[derive(Debug, PartialEq, Eq)]
enum CliCommand {
    Worker {
        config_path: Option<PathBuf>,
    },
    Query {
        config_path: Option<PathBuf>,
        query: String,
        top_k: Option<usize>,
    },
    Backup {
        config_path: Option<PathBuf>,
        destination: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("argument error: {0}")]
    Argument(String),
    #[error("configuration: {0}")]
    Config(#[from] ohara::config::ConfigError),
    #[error("worker: {0}")]
    Boot(#[from] ohara::BootError),
    #[error("query: {0}")]
    Query(#[from] ohara::pipeline::QueryError),
    #[error("operator: {0}")]
    Ops(#[from] ohara::ops::OpsError),
}

fn main() -> ExitCode {
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Some(command)) => command,
        Err(err) => {
            eprintln!("ohara: {err}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

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

    let result = match command {
        CliCommand::Worker { config_path } => runtime
            .block_on(boot_and_run(config_path))
            .map_err(CliError::Boot),
        CliCommand::Query {
            config_path,
            query,
            top_k,
        } => runtime.block_on(query_and_print(config_path, &query, top_k)),
        CliCommand::Backup {
            config_path,
            destination,
        } => backup_and_print(config_path.as_deref(), &destination),
    };

    match result {
        Ok(()) => {
            eprintln!("ohara: command complete");
            ExitCode::SUCCESS
        }
        Err(err) => {
            print_error_chain(&err);
            ExitCode::FAILURE
        }
    }
}

fn parse_args<I>(args: I) -> Result<Option<CliCommand>, CliError>
where
    I: IntoIterator<Item = String>,
{
    let mut config_path = None;
    let mut query_mode = false;
    let mut backup_mode = false;
    let mut query_parts = Vec::new();
    let mut backup_destination = None;
    let mut top_k = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--config" => {
                let path = args
                    .next()
                    .ok_or_else(|| CliError::Argument("--config requires a path".to_string()))?;
                config_path = Some(PathBuf::from(path));
            }
            "--top-k" => {
                let value = args.next().ok_or_else(|| {
                    CliError::Argument("--top-k requires a positive integer".to_string())
                })?;
                let parsed = value.parse::<usize>().map_err(|_| {
                    CliError::Argument(format!("--top-k must be a positive integer, got {value:?}"))
                })?;
                if parsed == 0 {
                    return Err(CliError::Argument(
                        "--top-k must be greater than zero".to_string(),
                    ));
                }
                top_k = Some(parsed);
            }
            "query" if !query_mode && !backup_mode && query_parts.is_empty() => {
                query_mode = true;
            }
            "backup" if !query_mode && !backup_mode && query_parts.is_empty() => {
                backup_mode = true;
            }
            value if query_mode => {
                if value.starts_with('-') {
                    return Err(CliError::Argument(format!(
                        "unknown query option {value:?}"
                    )));
                }
                query_parts.push(value.to_string());
            }
            value if backup_mode => {
                if value.starts_with('-') {
                    return Err(CliError::Argument(format!(
                        "unknown backup option {value:?}"
                    )));
                }
                if backup_destination.is_some() {
                    return Err(CliError::Argument(
                        "backup accepts one destination directory".to_string(),
                    ));
                }
                backup_destination = Some(value.to_string());
            }
            value => {
                return Err(CliError::Argument(format!("unknown argument {value:?}")));
            }
        }
    }

    if query_mode {
        let query = query_parts.join(" ");
        if query.trim().is_empty() {
            return Err(CliError::Argument(
                "query requires non-empty text".to_string(),
            ));
        }
        Ok(Some(CliCommand::Query {
            config_path,
            query,
            top_k,
        }))
    } else if backup_mode {
        if top_k.is_some() {
            return Err(CliError::Argument(
                "--top-k is only valid with the query command".to_string(),
            ));
        }
        let destination = backup_destination.ok_or_else(|| {
            CliError::Argument("backup requires a destination directory".to_string())
        })?;
        Ok(Some(CliCommand::Backup {
            config_path,
            destination: PathBuf::from(destination),
        }))
    } else if top_k.is_some() {
        Err(CliError::Argument(
            "--top-k is only valid with the query command".to_string(),
        ))
    } else {
        Ok(Some(CliCommand::Worker { config_path }))
    }
}

async fn boot_and_run(config_path: Option<PathBuf>) -> Result<(), ohara::BootError> {
    let config = ohara::config::Config::load(config_path.as_deref())?;
    ohara::run(config).await
}

async fn query_and_print(
    config_path: Option<PathBuf>,
    query: &str,
    top_k: Option<usize>,
) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path.as_deref())?;
    let top_k = top_k.unwrap_or(config.retrieval().top_k());
    let results = ohara::pipeline::query(config, query, top_k).await?;
    if results.is_empty() {
        println!("no results");
        return Ok(());
    }
    for (rank, result) in results.iter().enumerate() {
        println!(
            "{}. score={:.4} citation={}",
            rank + 1,
            result.score,
            result.chunk_id
        );
        println!("   {}", result.text.replace('\n', " "));
    }
    Ok(())
}

fn backup_and_print(config_path: Option<&Path>, destination: &Path) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path)?;
    let report = ohara::ops::backup(&config, destination)?;
    println!("backup written to {}", report.destination().display());
    Ok(())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{CliCommand, parse_args};
    use std::path::PathBuf;

    #[test]
    fn parses_worker_defaults_and_config() {
        assert_eq!(
            parse_args(["--config", "ohara.toml"].into_iter().map(str::to_string)).unwrap(),
            Some(CliCommand::Worker {
                config_path: Some(PathBuf::from("ohara.toml")),
            })
        );
    }

    #[test]
    fn parses_query_text_and_optional_flags_in_any_order() {
        assert_eq!(
            parse_args(
                [
                    "--top-k", "3", "query", "how", "does", "WAL", "--config", "x.toml"
                ]
                .into_iter()
                .map(str::to_string),
            )
            .unwrap(),
            Some(CliCommand::Query {
                config_path: Some(PathBuf::from("x.toml")),
                query: "how does WAL".to_string(),
                top_k: Some(3),
            })
        );
    }

    #[test]
    fn rejects_missing_query_and_invalid_top_k() {
        assert!(parse_args(["query"].into_iter().map(str::to_string)).is_err());
        assert!(
            parse_args(
                ["query", "wal", "--top-k", "0"]
                    .into_iter()
                    .map(str::to_string)
            )
            .is_err()
        );
    }

    #[test]
    fn parses_backup_destination_and_rejects_missing_destination() {
        assert_eq!(
            parse_args(
                ["backup", "/tmp/ohara-backup", "--config", "x.toml"]
                    .into_iter()
                    .map(str::to_string),
            )
            .unwrap(),
            Some(CliCommand::Backup {
                config_path: Some(PathBuf::from("x.toml")),
                destination: PathBuf::from("/tmp/ohara-backup"),
            })
        );
        assert!(parse_args(["backup"].into_iter().map(str::to_string)).is_err());
    }
}
