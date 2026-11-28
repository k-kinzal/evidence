//! Serve one evidence peer over TCP.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// The worktree or bare peer directory.
    #[arg(short = 'C', long, default_value = ".")]
    directory: PathBuf,
    /// Address and port to listen on.
    #[arg(long, default_value = "127.0.0.1:7467")]
    listen: String,
}

fn main() -> evidence_core::Result<()> {
    let args = Args::parse();
    let dir =
        std::fs::canonicalize(&args.directory).map_err(|source| evidence_core::Error::Io {
            path: args.directory,
            source,
        })?;
    let filter = tracing_subscriber::EnvFilter::try_from_env("EVIDENCE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    evidence_core::distributed::serve(&dir, &args.listen)
}
