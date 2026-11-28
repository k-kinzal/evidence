//! Command-line definition.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Track where every file comes from, whether it is still up to date, and which parts of
/// hand-written documents are backed by evidence.
#[derive(Debug, Parser)]
#[command(name = "evidence", version, about, long_about = None, propagate_version = true, disable_help_subcommand = true,
    after_help = "Extensions on PATH: evidence <name> runs evidence-<name>.")]
pub(crate) struct Cli {
    /// Run as if started in this directory.
    #[arg(short = 'C', long, global = true, value_name = "DIR")]
    pub(crate) directory: Option<PathBuf>,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// When to use colors.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub(crate) color: ColorChoice,
    /// More logging (repeat for more).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub(crate) verbose: u8,
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ColorChoice {
    /// Colors on terminals, unless `NO_COLOR` is set.
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create `.evidence/` in a directory (default: the current one).
    Init {
        /// Project root.
        path: Option<PathBuf>,
        /// Create a peer store without a worktree.
        #[arg(long)]
        bare: bool,
    },
    /// Create and update a working copy from a peer.
    Clone {
        remote: String,
        destination: Option<PathBuf>,
    },
    /// Send recorded versions and shared objects to a configured peer.
    Push { remote: Option<String> },
    /// List branches, create one, or delete a local branch.
    Branch {
        name: Option<String>,
        #[arg(short = 'd', long, requires = "name")]
        delete: bool,
    },
    /// Switch to a branch.
    Switch { branch: String },
    /// Integrate a branch into the current one.
    Merge { branch: String },
    /// Write conflict markers or take a recorded side.
    Resolve {
        id: String,
        /// A parent version, ours, or theirs.
        #[arg(long)]
        take: Option<String>,
    },
    /// Restore an earlier version by recording a new version.
    Revert { target: String },
    /// Show the version history of an evidence ID.
    Log {
        id: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long, conflicts_with = "branch")]
        all: bool,
    },
    /// Compare recorded versions or a version and the worktree.
    Diff {
        target: String,
        other: Option<String>,
        #[arg(long, conflicts_with = "subject")]
        definition: bool,
        #[arg(long)]
        subject: bool,
        #[arg(long)]
        stat: bool,
    },
    /// Verify and clean up the local object store.
    Gc,
    /// Recover, restore, and refresh evidence in dependency order.
    Update(UpdateArgs),
    /// Show evidence that needs attention and project diagnostics.
    Status,
    /// Show the evidence graph or query it with GQL.
    Graph {
        /// A read-only GQL query.
        query: Option<String>,
    },
    /// Inspect or change configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    #[command(external_subcommand)]
    Extension(Vec<OsString>),
}

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Ids or paths. By default, refresh every production that is due or stale.
    pub(crate) targets: Vec<String>,
    /// Run every selected production regardless of interval and freshness.
    #[arg(long)]
    pub(crate) force: bool,
    /// Only run the selection; report dependents instead of re-running them.
    #[arg(long)]
    pub(crate) isolated: bool,
    /// Work with local records only.
    #[arg(long)]
    pub(crate) offline: bool,
    /// Fetch the named branch. Only the current branch is applied.
    #[arg(long, conflicts_with = "pin")]
    pub(crate) branch: Option<String>,
    /// Record the selected subjects and claims without running productions.
    #[arg(long, num_args = 1.., value_name = "TARGET", conflicts_with_all = ["targets", "force", "isolated", "jobs"])]
    pub(crate) pin: Vec<String>,
    /// Fetch records and show what would change without restoring or executing.
    #[arg(long, short = 'n')]
    pub(crate) dry_run: bool,
    /// Parallel jobs.
    #[arg(long, short)]
    pub(crate) jobs: Option<usize>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Every effective value.
    List {
        /// Show which file each value comes from.
        #[arg(long)]
        show_origin: bool,
    },
    /// One value.
    Get {
        /// Dotted key, e.g. `execution.jobs`.
        key: String,
    },
    /// Set a value (project file by default).
    Set {
        /// Dotted key.
        key: String,
        /// Value (TOML syntax, or a bare string).
        value: String,
        /// Write to the global file.
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Write to `.evidence/config.local.toml`.
        #[arg(long)]
        local: bool,
    },
    /// Paths of the configuration files.
    Path,
}
