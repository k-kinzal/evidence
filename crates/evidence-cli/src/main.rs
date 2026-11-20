//! The `evidence` command.
#![forbid(unsafe_code)]

mod cli;
mod commands;
mod output;

use clap::Parser;

fn main() -> std::process::ExitCode {
    let cli = cli::Cli::parse();
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
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
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
            // 2 = the command itself could not run (usage, I/O, configuration).
            std::process::ExitCode::from(2)
        }
    }
}
