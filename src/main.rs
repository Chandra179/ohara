//! Binary entry point — the CLI module owns parsing and dispatch.

use std::process::ExitCode;

mod cli;

fn main() -> ExitCode {
    cli::run(std::env::args().skip(1))
}
