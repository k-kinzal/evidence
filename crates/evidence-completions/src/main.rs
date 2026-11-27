//! Generates shell completion scripts that query the installed `evidence` binary.

use std::io::{self, Write as _};

use clap::{Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(version, about = "Generate dynamic shell completion for Evidence")]
struct Args {
    /// Shell to generate completion for.
    #[arg(value_enum)]
    shell: Shell,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Fish,
}

fn main() -> io::Result<()> {
    let script = match Args::parse().shell {
        Shell::Bash => include_str!("evidence.bash"),
        Shell::Zsh => include_str!("_evidence"),
        Shell::Fish => include_str!("evidence.fish"),
    };
    io::stdout().lock().write_all(script.as_bytes())
}
