//! Command-line definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use evidence_core::format::{Kind, Lifecycle};

/// Track where every file comes from, whether it is still up to date, and which parts of
/// hand-written documents are backed by evidence.
#[derive(Debug, Parser)]
#[command(name = "evidence", version, about, long_about = None, propagate_version = true)]
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
    },
    /// Scaffold an evidence file for a subject.
    Add(AddArgs),
    /// Remove a pair (evidence file and subject).
    #[command(visible_alias = "rm")]
    Remove {
        /// Id or path.
        target: String,
        /// Keep the subject; only remove the evidence file.
        #[arg(long)]
        keep_subject: bool,
        /// Remove even if other evidence depends on it.
        #[arg(long)]
        force: bool,
    },
    /// Refresh evidence: due and stale pairs, everything with --force, or only the given paths.
    Update(UpdateArgs),
    /// List pairs that need attention (due, stale, modified, failed, missing, broken).
    Outdated {
        /// Ids or paths (default: the whole project).
        targets: Vec<String>,
        /// Exit with an error if any pair has one of these states.
        #[arg(long, value_enum, value_delimiter = ',')]
        deny: Vec<HealthArg>,
    },
    /// Overview of every pair.
    Status {
        /// Ids or paths (default: the whole project).
        targets: Vec<String>,
    },
    /// Confirm authored subjects against current evidence and pin their claims.
    Review {
        /// Ids or paths of authored subjects.
        #[arg(required = true)]
        targets: Vec<String>,
        /// Only these claims.
        #[arg(long = "claim", value_name = "ID")]
        claims: Vec<String>,
        /// Reviewer recorded in the state (default: `user.name` from the config).
        #[arg(long)]
        by: Option<String>,
    },
    /// Validate configuration, evidence files, the programs they run, schemas, and claims.
    Validate {
        /// Ids or paths (default: the whole project).
        targets: Vec<String>,
        /// Treat warnings as errors.
        #[arg(long)]
        strict: bool,
    },
    /// Line-level coverage of an authored subject by its claims.
    Coverage {
        /// Id or path.
        target: String,
        /// Fail if the covered-or-primary share is below this percentage.
        #[arg(long, value_name = "PERCENT")]
        min: Option<f64>,
        /// Print every line with its status.
        #[arg(long)]
        lines: bool,
    },
    /// Show the dependency tree (or, with --invert, everything affected by a change).
    Tree {
        /// Id or path (default: all roots).
        target: Option<String>,
        /// Show dependents instead of dependencies (impact).
        #[arg(short, long)]
        invert: bool,
        /// Maximum depth.
        #[arg(long)]
        depth: Option<usize>,
        /// Output format.
        #[arg(long, value_enum, default_value_t = TreeFormat::Text)]
        format: TreeFormat,
    },
    /// Explain why a pair is due, stale, or broken.
    Why {
        /// Id or path.
        target: String,
    },
    /// Show a pair: definition, state, locator, status, and neighbors.
    Show {
        /// Id or path.
        target: String,
    },
    /// Past runs and reviews of a pair.
    History {
        /// Id or path.
        target: String,
        /// Number of most recent events.
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },
    /// Full-text search across subjects and evidence.
    Search {
        /// Terms (all must match).
        #[arg(required = true)]
        query: Vec<String>,
        /// Maximum number of results.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Scan files instead of using the SQLite index.
        #[arg(long)]
        no_index: bool,
    },
    /// Print a stored snapshot (`sha256:…`, optionally `:path` inside a directory snapshot).
    Cat {
        /// Snapshot id, optionally followed by `:path`.
        object: String,
    },
    /// Object store maintenance.
    #[command(subcommand)]
    Store(StoreCommand),
    /// Optional SQLite index.
    #[command(subcommand)]
    Index(IndexCommand),
    /// Inspect or change configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Print a JSON Schema.
    Schema {
        /// Which schema.
        #[arg(value_enum)]
        which: SchemaKind,
    },
    /// Generate shell completions.
    Completions {
        /// Shell.
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Subject path (file or directory).
    pub(crate) subject: PathBuf,
    /// Where to write the evidence file (default: `<subject>.evid` next to the subject). Must
    /// be inside the project; the subject may live elsewhere.
    #[arg(long, value_name = "PATH")]
    pub(crate) evid: Option<PathBuf>,
    /// Evidence kind.
    #[arg(long, value_parser = parse_kind)]
    pub(crate) kind: Kind,
    /// Id (default: derived from the file name).
    #[arg(long)]
    pub(crate) id: Option<String>,
    /// Title.
    #[arg(long)]
    pub(crate) title: Option<String>,
    /// Capture the command's standard output as the subject (`stdout = true`).
    #[arg(long)]
    pub(crate) stdout: bool,
    /// Input evidence id (derived). Repeatable.
    #[arg(long = "input", value_name = "ID")]
    pub(crate) inputs: Vec<String>,
    /// Refresh interval for sources, e.g. `6h`.
    #[arg(long)]
    pub(crate) interval: Option<String>,
    /// Lifecycle (authored/derived).
    #[arg(long, value_parser = parse_lifecycle)]
    pub(crate) lifecycle: Option<Lifecycle>,
    /// Tag. Repeatable.
    #[arg(long = "tag")]
    pub(crate) tags: Vec<String>,
    /// The command (after `--`): `[source]`, `[transform]`, or an authored `[generator]`.
    /// Placeholders such as `{output}` and `{input.NAME}` are expanded when it runs.
    #[arg(last = true, value_name = "COMMAND")]
    pub(crate) run: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Ids or paths. Without them, everything due or stale is updated.
    pub(crate) targets: Vec<String>,
    /// Run every selected pair regardless of interval and freshness.
    #[arg(long)]
    pub(crate) force: bool,
    /// Only update the selection; mark dependents stale instead of re-running them.
    #[arg(long)]
    pub(crate) isolated: bool,
    /// Show what would run, and why, without running anything.
    #[arg(long, short = 'n')]
    pub(crate) dry_run: bool,
    /// Parallel jobs.
    #[arg(long, short)]
    pub(crate) jobs: Option<usize>,
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub(crate) enum StoreCommand {
    /// Re-hash every object and report corruption.
    Verify,
    /// Remove objects no evidence file (or history entry) refers to.
    Prune {
        /// Also drop snapshots only referenced by history.
        #[arg(long)]
        drop_history: bool,
    },
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub(crate) enum IndexCommand {
    /// Recreate `.evidence/index.db` from the files.
    Rebuild,
    /// Bring the index up to date (incremental).
    Sync,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum SchemaKind {
    /// Evidence file (`*.evid`).
    Evid,
    /// Configuration file.
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TreeFormat {
    /// Indented text.
    Text,
    /// Graphviz DOT (whole graph).
    Dot,
    /// JSON.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum HealthArg {
    /// Interval elapsed.
    Due,
    /// Inputs/evidence changed.
    Stale,
    /// Changed outside the tool.
    Modified,
    /// Last run failed.
    Failed,
    /// Subject missing.
    Missing,
    /// Broken references.
    Broken,
}

impl HealthArg {
    pub(crate) fn matches(self, health: evidence_core::Health) -> bool {
        use evidence_core::Health;
        matches!(
            (self, health),
            (Self::Due, Health::Due)
                | (Self::Stale, Health::Stale)
                | (Self::Modified, Health::Modified)
                | (Self::Failed, Health::Failed)
                | (Self::Missing, Health::Missing)
                | (Self::Broken, Health::Broken)
        )
    }
}

fn parse_kind(s: &str) -> Result<Kind, String> {
    s.parse()
}

fn parse_lifecycle(s: &str) -> Result<Lifecycle, String> {
    match s {
        "snapshot" => Ok(Lifecycle::Snapshot),
        "living" => Ok(Lifecycle::Living),
        other => Err(format!(
            "unknown lifecycle `{other}` (expected snapshot or living)"
        )),
    }
}
