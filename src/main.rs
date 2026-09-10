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
    ohara requeue --doc <id> [--config <path.toml>]
    ohara archive <id> [--config <path.toml>]
    ohara delete <id> [--config <path.toml>]
    ohara prune [--dry-run] [--config <path.toml>]
    ohara metrics [--json] [--config <path.toml>]
    ohara er merge [--config <path.toml>]

OPTIONS:
    --config <path>    TOML config; unset knobs fall back to defaults
    --top-k <n>        Number of ranked chunks to print for query (config default)
    query <text>       Retrieve ranked chunks with chunk-id citations
    backup <directory> Write a consistent, non-overwriting snapshot
    requeue --doc <id> Reset failed or interrupted jobs for a document
    archive <id>       Retain a document's chunks but stop future work
    delete <id>        Request knowledge-first document deletion
    prune              Apply configured raw-payload retention policy
    --dry-run          Show prune selection without deleting files
    metrics            Show queue, audit, entity-review, recrawl, and raw usage metrics
    --json             Render metrics as machine-readable JSON
    er merge           Execute pending offline entity merges
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
    Requeue {
        config_path: Option<PathBuf>,
        doc_id: String,
    },
    Archive {
        config_path: Option<PathBuf>,
        doc_id: String,
    },
    Delete {
        config_path: Option<PathBuf>,
        doc_id: String,
    },
    Prune {
        config_path: Option<PathBuf>,
        dry_run: bool,
    },
    Metrics {
        config_path: Option<PathBuf>,
        json: bool,
    },
    EntityMerge {
        config_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAction {
    Requeue,
    Archive,
    Delete,
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
    #[error("output: {0}")]
    Output(#[from] serde_json::Error),
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
        CliCommand::Requeue {
            config_path,
            doc_id,
        } => requeue_and_print(config_path.as_deref(), &doc_id),
        CliCommand::Archive {
            config_path,
            doc_id,
        } => archive_and_print(config_path.as_deref(), &doc_id),
        CliCommand::Delete {
            config_path,
            doc_id,
        } => delete_and_print(config_path.as_deref(), &doc_id),
        CliCommand::Prune {
            config_path,
            dry_run,
        } => prune_and_print(config_path.as_deref(), dry_run),
        CliCommand::Metrics { config_path, json } => {
            metrics_and_print(config_path.as_deref(), json)
        }
        CliCommand::EntityMerge { config_path } => runtime.block_on(merge_and_print(config_path)),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    Query,
    Backup,
    Lifecycle(LifecycleAction),
    Prune,
    Metrics,
    EntityMerge,
}

#[derive(Debug, Default)]
struct CliParser {
    config_path: Option<PathBuf>,
    mode: Option<CliMode>,
    query_parts: Vec<String>,
    backup_destination: Option<String>,
    lifecycle_doc: Option<String>,
    top_k: Option<usize>,
    dry_run: bool,
    json: bool,
    entity_group: bool,
}

fn parse_args<I>(args: I) -> Result<Option<CliCommand>, CliError>
where
    I: IntoIterator<Item = String>,
{
    let mut parser = CliParser::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "--help" | "-h") {
            return Ok(None);
        }
        parser.parse_arg(&arg, &mut args)?;
    }
    parser.finish()
}

impl CliParser {
    fn parse_arg<I>(&mut self, arg: &str, args: &mut I) -> Result<(), CliError>
    where
        I: Iterator<Item = String>,
    {
        match arg {
            "--config" => {
                self.config_path = Some(PathBuf::from(next_arg(args, "--config requires a path")?));
            }
            "--top-k" => {
                self.top_k = Some(parse_top_k(&next_arg(
                    args,
                    "--top-k requires a positive integer",
                )?)?);
            }
            "--doc" => self.parse_requeue_doc(args)?,
            "query" => self.select_mode(CliMode::Query)?,
            "backup" => self.select_mode(CliMode::Backup)?,
            "requeue" => self.select_mode(CliMode::Lifecycle(LifecycleAction::Requeue))?,
            "archive" => self.select_mode(CliMode::Lifecycle(LifecycleAction::Archive))?,
            "delete" => self.select_mode(CliMode::Lifecycle(LifecycleAction::Delete))?,
            "prune" => self.select_mode(CliMode::Prune)?,
            "metrics" => self.select_mode(CliMode::Metrics)?,
            "er" => self.select_entity_group()?,
            "merge" => self.select_entity_merge()?,
            "--dry-run" => self.parse_dry_run()?,
            "--json" => self.parse_json()?,
            value => self.parse_value(value)?,
        }
        Ok(())
    }

    fn select_mode(&mut self, mode: CliMode) -> Result<(), CliError> {
        if self.mode.is_some() || !self.query_parts.is_empty() || self.entity_group {
            return Err(CliError::Argument(
                "multiple commands are not supported".to_string(),
            ));
        }
        self.mode = Some(mode);
        Ok(())
    }

    fn select_entity_group(&mut self) -> Result<(), CliError> {
        if self.mode.is_some() || !self.query_parts.is_empty() || self.entity_group {
            return Err(CliError::Argument(
                "multiple commands are not supported".to_string(),
            ));
        }
        self.entity_group = true;
        Ok(())
    }

    fn select_entity_merge(&mut self) -> Result<(), CliError> {
        if !self.entity_group || self.mode.is_some() {
            return Err(CliError::Argument(
                "expected `er merge` as a command".to_string(),
            ));
        }
        self.entity_group = false;
        self.mode = Some(CliMode::EntityMerge);
        Ok(())
    }

    fn parse_requeue_doc<I>(&mut self, args: &mut I) -> Result<(), CliError>
    where
        I: Iterator<Item = String>,
    {
        if self.mode != Some(CliMode::Lifecycle(LifecycleAction::Requeue)) {
            return Err(CliError::Argument(
                "--doc is only valid with requeue".to_string(),
            ));
        }
        let doc_id = next_arg(args, "--doc requires an id")?;
        self.set_lifecycle_doc(doc_id)
    }

    fn parse_dry_run(&mut self) -> Result<(), CliError> {
        if self.mode != Some(CliMode::Prune) {
            return Err(CliError::Argument(
                "--dry-run is only valid with prune".to_string(),
            ));
        }
        if self.dry_run {
            return Err(CliError::Argument(
                "prune accepts --dry-run at most once".to_string(),
            ));
        }
        self.dry_run = true;
        Ok(())
    }

    fn parse_json(&mut self) -> Result<(), CliError> {
        if self.mode != Some(CliMode::Metrics) {
            return Err(CliError::Argument(
                "--json is only valid with metrics".to_string(),
            ));
        }
        if self.json {
            return Err(CliError::Argument(
                "metrics accepts --json at most once".to_string(),
            ));
        }
        self.json = true;
        Ok(())
    }

    fn parse_value(&mut self, value: &str) -> Result<(), CliError> {
        match self.mode {
            Some(CliMode::Query) => {
                if value.starts_with('-') {
                    return Err(CliError::Argument(format!(
                        "unknown query option {value:?}"
                    )));
                }
                self.query_parts.push(value.to_string());
            }
            Some(CliMode::Backup) => {
                if value.starts_with('-') {
                    return Err(CliError::Argument(format!(
                        "unknown backup option {value:?}"
                    )));
                }
                if self.backup_destination.replace(value.to_string()).is_some() {
                    return Err(CliError::Argument(
                        "backup accepts one destination directory".to_string(),
                    ));
                }
            }
            Some(CliMode::Lifecycle(_)) => {
                if value.starts_with('-') {
                    return Err(CliError::Argument(format!(
                        "unknown lifecycle option {value:?}"
                    )));
                }
                self.set_lifecycle_doc(value.to_string())?;
            }
            Some(CliMode::Prune) => {
                return Err(CliError::Argument(
                    "prune does not accept positional arguments".to_string(),
                ));
            }
            Some(CliMode::Metrics) => {
                return Err(CliError::Argument(
                    "metrics does not accept positional arguments".to_string(),
                ));
            }
            Some(CliMode::EntityMerge) => {
                return Err(CliError::Argument(
                    "er merge does not accept positional arguments".to_string(),
                ));
            }
            None if self.entity_group => {
                return Err(CliError::Argument(
                    "expected `merge` after `er`".to_string(),
                ));
            }
            None => return Err(CliError::Argument(format!("unknown argument {value:?}"))),
        }
        Ok(())
    }

    fn set_lifecycle_doc(&mut self, doc_id: String) -> Result<(), CliError> {
        if doc_id.starts_with('-') {
            return Err(CliError::Argument(
                "document id must not start with '-'".to_string(),
            ));
        }
        if self.lifecycle_doc.replace(doc_id).is_some() {
            return Err(CliError::Argument(
                "lifecycle command accepts one document id".to_string(),
            ));
        }
        Ok(())
    }

    fn finish(self) -> Result<Option<CliCommand>, CliError> {
        match self.mode {
            Some(CliMode::Query) => self.finish_query(),
            Some(CliMode::Backup) => self.finish_backup(),
            Some(CliMode::Lifecycle(action)) => self.finish_lifecycle(action),
            Some(CliMode::Prune) => self.finish_prune(),
            Some(CliMode::Metrics) => self.finish_metrics(),
            Some(CliMode::EntityMerge) => self.finish_entity_merge(),
            None if self.entity_group => Err(CliError::Argument(
                "expected `merge` after `er`".to_string(),
            )),
            None if self.top_k.is_some() => Err(CliError::Argument(
                "--top-k is only valid with the query command".to_string(),
            )),
            None if self.json => Err(CliError::Argument(
                "--json is only valid with the metrics command".to_string(),
            )),
            None => Ok(Some(CliCommand::Worker {
                config_path: self.config_path,
            })),
        }
    }

    fn finish_query(self) -> Result<Option<CliCommand>, CliError> {
        reject_json(self.json)?;
        let query = self.query_parts.join(" ");
        if query.trim().is_empty() {
            return Err(CliError::Argument(
                "query requires non-empty text".to_string(),
            ));
        }
        Ok(Some(CliCommand::Query {
            config_path: self.config_path,
            query,
            top_k: self.top_k,
        }))
    }

    fn finish_backup(self) -> Result<Option<CliCommand>, CliError> {
        reject_top_k(self.top_k)?;
        reject_json(self.json)?;
        let destination = self.backup_destination.ok_or_else(|| {
            CliError::Argument("backup requires a destination directory".to_string())
        })?;
        Ok(Some(CliCommand::Backup {
            config_path: self.config_path,
            destination: PathBuf::from(destination),
        }))
    }

    fn finish_lifecycle(self, action: LifecycleAction) -> Result<Option<CliCommand>, CliError> {
        reject_top_k(self.top_k)?;
        reject_json(self.json)?;
        let doc_id = self.lifecycle_doc.ok_or_else(|| {
            CliError::Argument("lifecycle command requires a document id".to_string())
        })?;
        let command = match action {
            LifecycleAction::Requeue => CliCommand::Requeue {
                config_path: self.config_path,
                doc_id,
            },
            LifecycleAction::Archive => CliCommand::Archive {
                config_path: self.config_path,
                doc_id,
            },
            LifecycleAction::Delete => CliCommand::Delete {
                config_path: self.config_path,
                doc_id,
            },
        };
        Ok(Some(command))
    }

    fn finish_prune(self) -> Result<Option<CliCommand>, CliError> {
        reject_top_k(self.top_k)?;
        reject_json(self.json)?;
        if self.lifecycle_doc.is_some() || self.backup_destination.is_some() {
            return Err(CliError::Argument(
                "prune does not accept a document id or destination".to_string(),
            ));
        }
        Ok(Some(CliCommand::Prune {
            config_path: self.config_path,
            dry_run: self.dry_run,
        }))
    }

    fn finish_metrics(self) -> Result<Option<CliCommand>, CliError> {
        reject_top_k(self.top_k)?;
        if self.lifecycle_doc.is_some() || self.backup_destination.is_some() {
            return Err(CliError::Argument(
                "metrics does not accept a document id or destination".to_string(),
            ));
        }
        Ok(Some(CliCommand::Metrics {
            config_path: self.config_path,
            json: self.json,
        }))
    }

    fn finish_entity_merge(self) -> Result<Option<CliCommand>, CliError> {
        reject_top_k(self.top_k)?;
        reject_json(self.json)?;
        if self.lifecycle_doc.is_some() || self.backup_destination.is_some() {
            return Err(CliError::Argument(
                "er merge does not accept positional arguments".to_string(),
            ));
        }
        Ok(Some(CliCommand::EntityMerge {
            config_path: self.config_path,
        }))
    }
}

fn next_arg<I>(args: &mut I, message: &str) -> Result<String, CliError>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| CliError::Argument(message.to_string()))
}

fn parse_top_k(value: &str) -> Result<usize, CliError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        CliError::Argument(format!("--top-k must be a positive integer, got {value:?}"))
    })?;
    if parsed == 0 {
        return Err(CliError::Argument(
            "--top-k must be greater than zero".to_string(),
        ));
    }
    Ok(parsed)
}

fn reject_top_k(top_k: Option<usize>) -> Result<(), CliError> {
    if top_k.is_some() {
        Err(CliError::Argument(
            "--top-k is only valid with the query command".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn reject_json(json: bool) -> Result<(), CliError> {
    if json {
        Err(CliError::Argument(
            "--json is only valid with the metrics command".to_string(),
        ))
    } else {
        Ok(())
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

fn requeue_and_print(config_path: Option<&Path>, doc_id: &str) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path)?;
    let report = ohara::ops::requeue(&config, doc_id)?;
    println!(
        "requeued {} job(s) for {}",
        report.jobs_reset, report.doc_id
    );
    Ok(())
}

fn archive_and_print(config_path: Option<&Path>, doc_id: &str) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path)?;
    let report = ohara::ops::archive(&config, doc_id)?;
    println!("archived {}", report.doc_id);
    Ok(())
}

fn delete_and_print(config_path: Option<&Path>, doc_id: &str) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path)?;
    let report = ohara::ops::delete_document(&config, doc_id)?;
    println!("deletion requested for {}", report.doc_id);
    Ok(())
}

fn prune_and_print(config_path: Option<&Path>, dry_run: bool) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path)?;
    let report = ohara::ops::prune(&config, dry_run)?;
    let action = if report.dry_run {
        "would prune"
    } else {
        "pruned"
    };
    let files = if report.dry_run {
        report.selected
    } else {
        report.deleted
    };
    let bytes = if report.dry_run {
        report.planned_bytes
    } else {
        report.reclaimed_bytes
    };
    println!(
        "{action} {files} file(s), {bytes} byte(s); skipped {} live document(s) and {} missing payload(s)",
        report.skipped_active, report.missing
    );
    if !report.policy_active {
        println!("no raw retention limits configured; nothing selected");
    }
    Ok(())
}

fn metrics_and_print(config_path: Option<&Path>, json: bool) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path)?;
    let report = ohara::ops::metrics(&config)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("captured at {}", report.control.captured_at);
    println!("documents:");
    for (status, count) in &report.control.documents_by_status {
        println!("  {status}: {count}");
    }
    println!("jobs:");
    for (stage, statuses) in &report.control.jobs_by_stage_status {
        let summary = statuses
            .iter()
            .map(|(status, count)| format!("{status}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  {stage}: {summary}");
    }
    println!("audit outcomes:");
    for (outcome, count) in &report.control.events_by_outcome {
        println!("  {outcome}: {count}");
    }
    println!("audit stages:");
    for (stage, count) in &report.control.events_by_stage {
        println!("  {stage}: {count}");
    }
    println!("pending ER reviews: {}", report.control.pending_er_reviews);
    println!(
        "documents due for recrawl: {}",
        report.control.due_for_recrawl
    );
    println!(
        "raw payloads: {} file(s), {} byte(s)",
        report.raw_files, report.raw_bytes
    );
    if let Some(limit) = report.raw_max_bytes {
        println!("raw byte limit: {limit}");
    }
    if let Some(limit) = report.raw_max_age_days {
        println!("raw age limit: {limit} day(s)");
    }
    Ok(())
}

async fn merge_and_print(config_path: Option<PathBuf>) -> Result<(), CliError> {
    let config = ohara::config::Config::load(config_path.as_deref())?;
    let report = ohara::ops::merge_entities(&config).await?;
    println!(
        "examined {} review(s), recorded {} merge(s), replayed {} fold(s)",
        report.reviews_examined, report.merges_recorded, report.folds_replayed
    );
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

    #[test]
    fn parses_lifecycle_commands() {
        assert_eq!(
            parse_args(
                ["requeue", "--doc", "doc-1", "--config", "x.toml"]
                    .into_iter()
                    .map(str::to_string),
            )
            .unwrap(),
            Some(CliCommand::Requeue {
                config_path: Some(PathBuf::from("x.toml")),
                doc_id: "doc-1".to_string(),
            })
        );
        assert_eq!(
            parse_args(["archive", "doc-1"].into_iter().map(str::to_string)).unwrap(),
            Some(CliCommand::Archive {
                config_path: None,
                doc_id: "doc-1".to_string(),
            })
        );
        assert_eq!(
            parse_args(["delete", "doc-1"].into_iter().map(str::to_string)).unwrap(),
            Some(CliCommand::Delete {
                config_path: None,
                doc_id: "doc-1".to_string(),
            })
        );
        assert!(parse_args(["requeue"].into_iter().map(str::to_string)).is_err());
    }

    #[test]
    fn parses_prune_and_dry_run() {
        assert_eq!(
            parse_args(
                ["prune", "--dry-run", "--config", "x.toml"]
                    .into_iter()
                    .map(str::to_string),
            )
            .unwrap(),
            Some(CliCommand::Prune {
                config_path: Some(PathBuf::from("x.toml")),
                dry_run: true,
            })
        );
        assert!(parse_args(["--dry-run"].into_iter().map(str::to_string)).is_err());
        assert!(
            parse_args(
                ["prune", "--dry-run", "--dry-run"]
                    .into_iter()
                    .map(str::to_string)
            )
            .is_err()
        );
    }

    #[test]
    fn parses_metrics_with_optional_json_output() {
        assert_eq!(
            parse_args(
                ["metrics", "--json", "--config", "x.toml"]
                    .into_iter()
                    .map(str::to_string),
            )
            .unwrap(),
            Some(CliCommand::Metrics {
                config_path: Some(PathBuf::from("x.toml")),
                json: true,
            })
        );
        assert_eq!(
            parse_args(["metrics"].into_iter().map(str::to_string)).unwrap(),
            Some(CliCommand::Metrics {
                config_path: None,
                json: false,
            })
        );
        assert!(parse_args(["metrics", "extra"].into_iter().map(str::to_string)).is_err());
        assert!(parse_args(["--json"].into_iter().map(str::to_string)).is_err());
    }

    #[test]
    fn parses_entity_merge_command() {
        assert_eq!(
            parse_args(
                ["er", "merge", "--config", "x.toml"]
                    .into_iter()
                    .map(str::to_string),
            )
            .unwrap(),
            Some(CliCommand::EntityMerge {
                config_path: Some(PathBuf::from("x.toml")),
            })
        );
        assert!(parse_args(["er"].into_iter().map(str::to_string)).is_err());
        assert!(parse_args(["merge"].into_iter().map(str::to_string)).is_err());
    }
}
