//! The `evidence` command.
#![forbid(unsafe_code)]

mod cli;
mod commands;
mod extensions;
mod graph;
mod output;

use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    if extensions::introspect(&args[1..]) {
        return ExitCode::SUCCESS;
    }
    let cli = cli::Cli::parse_from(args);
    if let cli::Command::Extension(args) = &cli.command
        && let Some((name, args)) = args.split_first()
    {
        return match extensions::run(cli.directory.as_deref(), name, args) {
            Ok(status) => status,
            Err(error) => {
                warn_line!("error: {error:#}");
                ExitCode::from(2)
            }
        };
    }
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("EVIDENCE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
    let json = cli.json;
    match commands::run(cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            if json {
                let body = serde_json::json!({
                    "version": output::JSON_VERSION,
                    "error": { "message": format!("{err:#}") },
                });
                warn_line!("{body}");
            } else {
                warn_line!("error: {err:#}");
            }
            ExitCode::from(2)
        }
    }
}
