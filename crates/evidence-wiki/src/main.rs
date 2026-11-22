//! `evidence-wiki`: a wiki over an Evidence project.
#![forbid(unsafe_code)]

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use evidence_core::format::PageKind;
use evidence_wiki::check;
use evidence_wiki::scaffold::{self, InitOptions, NewPage};
use evidence_wiki::search::{self, Filter};
use evidence_wiki::server::{self, ServeOptions};
use evidence_wiki::{Wiki, agents, export, mcp};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "evidence-wiki",
    version,
    about = "A wiki over an Evidence project: browse, search, serve, build, and maintain it with agents"
)]
struct Cli {
    /// Project directory (any directory inside the project).
    #[arg(short = 'C', long = "project", global = true, default_value = ".")]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Kind {
    Article,
    Category,
    Term,
}

impl From<Kind> for PageKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Article => Self::Article,
            Kind::Category => Self::Category,
            Kind::Term => Self::Term,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Set up a wiki: `.evidence/wiki.toml`, a home page, and the skills in `.claude/skills`.
    Init {
        /// Directory for wiki pages.
        #[arg(long, default_value = "wiki")]
        pages: String,
        /// Do not install the skills.
        #[arg(long)]
        no_skills: bool,
    },
    /// Serve the wiki (pages, JSON API, MCP at /mcp) and run scheduled agents.
    Serve {
        /// Address, e.g. 127.0.0.1:4000 (default: `[server] addr`).
        #[arg(long)]
        addr: Option<String>,
        /// Do not run agents.
        #[arg(long)]
        no_agents: bool,
    },
    /// Write the wiki as a static site.
    Build {
        /// Output directory.
        #[arg(long, short, default_value = "_site")]
        out: PathBuf,
    },
    /// What the wiki lacks, and which skill fixes each finding.
    Check {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Exit with 1 when there are findings.
        #[arg(long)]
        strict: bool,
    },
    /// Search pages.
    Search {
        /// Words; "quoted phrases" stay together.
        #[arg(required = true)]
        query: Vec<String>,
        /// Maximum results.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Only pages in this category.
        #[arg(long)]
        category: Option<String>,
        /// Only pages with this tag.
        #[arg(long)]
        tag: Option<String>,
        /// Only pages under this path.
        #[arg(long)]
        under: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Lines matching a string (or a regular expression) across pages.
    Grep {
        /// The string or pattern.
        pattern: String,
        /// Treat the pattern as a regular expression.
        #[arg(long)]
        regex: bool,
        /// Only pages under this path.
        #[arg(long)]
        under: Option<String>,
        /// Maximum lines.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show a page: metadata, outline, and content (or one section).
    Show {
        /// Page id, title, alias, or path.
        page: String,
        /// Only this section (heading text or anchor).
        #[arg(long)]
        section: Option<String>,
        /// Only metadata and outline.
        #[arg(long)]
        outline: bool,
    },
    /// Create a page: the Markdown file (unless it exists) and its evidence file with `[wiki]`.
    New {
        /// Markdown file, e.g. wiki/terms/sbom.md.
        path: PathBuf,
        /// Title.
        #[arg(long)]
        title: String,
        /// Evidence id (default: from the path).
        #[arg(long)]
        id: Option<String>,
        /// Kind of page.
        #[arg(long, value_enum)]
        kind: Option<Kind>,
        /// Another name (repeatable).
        #[arg(long = "alias")]
        aliases: Vec<String>,
        /// A category (repeatable).
        #[arg(long = "category")]
        categories: Vec<String>,
        /// A tag (repeatable).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// One-line description.
        #[arg(long)]
        description: Option<String>,
        /// Author, e.g. ai:claude or human.
        #[arg(long)]
        author: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List or run the configured agents.
    Agents {
        #[command(subcommand)]
        action: Option<AgentsAction>,
    },
    /// Install or list the evidence-wiki skills.
    Skills {
        #[command(subcommand)]
        action: Option<SkillsAction>,
    },
    /// MCP server over stdio (for `claude mcp add`, editors, and agents).
    Mcp,
    /// Run one MCP tool from the command line, e.g. `tool links '{"page":"home"}'`.
    Tool {
        /// Tool name (`tool --list` lists them).
        #[arg(required_unless_present = "list")]
        name: Option<String>,
        /// Arguments as a JSON object.
        #[arg(default_value = "{}")]
        args: String,
        /// List the tools.
        #[arg(long)]
        list: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AgentsAction {
    /// Agents and their last runs.
    List,
    /// Run one agent now, in the foreground.
    Run {
        /// Agent name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum SkillsAction {
    /// Write the skills (default: <project>/.claude/skills).
    Install {
        /// Target directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Replace existing files.
        #[arg(long)]
        force: bool,
    },
    /// List the skills.
    List,
}

fn out(text: &str) -> Result<()> {
    let mut o = std::io::stdout().lock();
    o.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        o.write_all(b"\n")?;
    }
    Ok(())
}

fn out_json(v: &Value) -> Result<()> {
    out(&serde_json::to_string_pretty(v)?)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(std::io::stderr().lock(), "error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let dir = cli.project;
    match cli.command {
        Command::Init { pages, no_skills } => {
            let w = scaffold::init(
                &dir,
                &InitOptions {
                    pages,
                    skills: !no_skills,
                },
            )?;
            for f in &w.written {
                out(&format!("wrote {f}"))?;
            }
            for f in &w.kept {
                out(&format!("kept  {f}"))?;
            }
            out(
                "next: `evidence-wiki serve`, or `claude -p /evidence-wiki` to let an agent fill the wiki",
            )?;
        }
        Command::Serve { addr, no_agents } => {
            server::serve(
                &dir,
                &ServeOptions {
                    addr,
                    agents: !no_agents,
                },
            )?;
        }
        Command::Build { out: target } => {
            let wiki = Wiki::open(&dir)?;
            let target = if target.is_absolute() {
                target
            } else {
                std::env::current_dir()?.join(target)
            };
            let built = export::build(&wiki, &target)?;
            out(&format!(
                "wrote {} pages and {} files to {}",
                built.pages,
                built.files,
                built.out.display()
            ))?;
        }
        Command::Check { json, strict } => {
            let wiki = Wiki::open(&dir)?;
            let report = check::check(&wiki);
            if json {
                out_json(&serde_json::to_value(&report)?)?;
            } else {
                let s = &report.summary;
                out(&format!(
                    "{} pages ({} articles, {} categories, {} terms, {} sources); {} findings",
                    s.pages, s.articles, s.categories, s.terms, s.sources, s.findings
                ))?;
                for a in &report.actions {
                    out(&format!(
                        "  {:>4}  {:<20} /{:<30} {}",
                        a.count, a.finding, a.skill, a.why
                    ))?;
                }
                if !report.actions.is_empty() {
                    out("details: evidence-wiki check --json")?;
                }
            }
            if strict && report.summary.findings > 0 {
                return Ok(ExitCode::from(1));
            }
        }
        Command::Search {
            query,
            limit,
            category,
            tag,
            under,
            json,
        } => {
            let wiki = Wiki::open(&dir)?;
            let q = query.join(" ");
            let hits = search::search(
                &wiki,
                &q,
                &Filter {
                    category,
                    tag,
                    under,
                    kind: None,
                },
                limit,
            );
            if json {
                out_json(&json!({"query": q, "hits": hits}))?;
            } else {
                for h in &hits {
                    out(&format!(
                        "{:<32} {}  ({})\n    {}",
                        h.id, h.title, h.path, h.snippet
                    ))?;
                }
            }
        }
        Command::Grep {
            pattern,
            regex,
            under,
            limit,
            json,
        } => {
            let wiki = Wiki::open(&dir)?;
            let lines = search::grep(
                &wiki,
                &pattern,
                regex,
                &Filter {
                    under,
                    ..Filter::default()
                },
                0,
                limit,
            )?;
            if json {
                out_json(&json!({"lines": lines}))?;
            } else {
                for l in &lines {
                    out(&format!("{}:{}: {}", l.path, l.line, l.text))?;
                }
            }
        }
        Command::Show {
            page,
            section,
            outline,
        } => {
            let wiki = Wiki::open(&dir)?;
            let mut args = json!({"page": page, "outline_only": outline, "max_chars": 100_000});
            if let Some(s) = section {
                args["section"] = Value::String(s);
            }
            let (text, _) = mcp::tool(&wiki, "read_page", &args)?;
            out(&text)?;
        }
        Command::New {
            path,
            title,
            id,
            kind,
            aliases,
            categories,
            tags,
            description,
            author,
            json,
        } => {
            let created = scaffold::new_page(
                &std::fs::canonicalize(&dir).context("project directory")?,
                &NewPage {
                    path,
                    title,
                    id,
                    kind: kind.map(Into::into),
                    aliases,
                    categories,
                    tags,
                    description,
                    author,
                },
            )?;
            if json {
                out_json(&serde_json::to_value(&created)?)?;
            } else {
                out(&format!(
                    "created {} ({}) and {}",
                    created.page,
                    if created.existed { "kept" } else { "new" },
                    created.evid
                ))?;
            }
        }
        Command::Agents { action } => {
            let wiki = Wiki::open(&dir)?;
            let root = wiki.project().root().to_path_buf();
            match action.unwrap_or(AgentsAction::List) {
                AgentsAction::List => {
                    let records = agents::records(&root);
                    if wiki.config().agents.is_empty() {
                        out("no agents; add [[agents]] to .evidence/wiki.toml")?;
                    }
                    for a in &wiki.config().agents {
                        let last = records.get(&a.name).map_or_else(
                            || "never ran".to_owned(),
                            |r| {
                                format!(
                                    "last {} ({})",
                                    r.started.strftime("%Y-%m-%d %H:%M UTC"),
                                    if r.ok { "ok" } else { "failed" }
                                )
                            },
                        );
                        out(&format!(
                            "{:<16} every {:<8} {}  {}",
                            a.name,
                            a.interval.map_or_else(|| "-".to_owned(), |i| i.to_string()),
                            last,
                            a.run.join(" ")
                        ))?;
                    }
                }
                AgentsAction::Run { name } => {
                    let agent = wiki
                        .config()
                        .agents
                        .iter()
                        .find(|a| a.name == name)
                        .with_context(|| format!("no agent `{name}` in .evidence/wiki.toml"))?;
                    let record =
                        agents::run(&root, agent, "manual", &std::collections::BTreeMap::new())?;
                    out(&format!(
                        "{} ({}); log: {}",
                        if record.ok { "ok" } else { "failed" },
                        record
                            .exit
                            .map_or_else(|| "no exit code".to_owned(), |c| format!("exit {c}")),
                        record.log
                    ))?;
                    if !record.ok {
                        return Ok(ExitCode::from(1));
                    }
                }
            }
        }
        Command::Skills { action } => match action.unwrap_or(SkillsAction::List) {
            SkillsAction::List => {
                for (path, text) in scaffold::SKILLS {
                    if let Some(name) = path.strip_suffix("/SKILL.md") {
                        let desc = text
                            .lines()
                            .find_map(|l| l.strip_prefix("description:"))
                            .unwrap_or("")
                            .trim();
                        out(&format!(
                            "/{name:<32} {}",
                            evidence_wiki::wiki::truncate_chars(desc, 100)
                        ))?;
                    }
                }
            }
            SkillsAction::Install { dir: target, force } => {
                let root = evidence_core::Project::open(&dir)
                    .map_or_else(|_| dir.clone(), |p| p.root().to_path_buf());
                let target = target.unwrap_or_else(|| root.join(".claude").join("skills"));
                let w = scaffold::install_skills(&root, &target, force)?;
                for f in &w.written {
                    out(&format!("wrote {f}"))?;
                }
                for f in &w.kept {
                    out(&format!("kept  {f} (use --force to replace)"))?;
                }
            }
        },
        Command::Mcp => stdio(&dir)?,
        Command::Tool { name, args, list } => {
            if list {
                for n in mcp::tool_names() {
                    out(&n)?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            let wiki = Wiki::open(&dir)?;
            let args: Value =
                serde_json::from_str(&args).context("arguments must be a JSON object")?;
            let (text, _) = mcp::tool(&wiki, name.as_deref().unwrap_or(""), &args)?;
            out(&text)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// MCP over stdio: one JSON-RPC message per line.
fn stdio(dir: &std::path::Path) -> Result<()> {
    let mut wiki = Wiki::open(dir)?;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Err(e) = wiki.refresh() {
            let _ = writeln!(
                std::io::stderr().lock(),
                "evidence-wiki: reload failed: {e:#}"
            );
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => mcp::respond_any(&wiki, &msg),
            Err(e) => Some(mcp::parse_error(&e)),
        };
        if let Some(r) = reply {
            let mut o = std::io::stdout().lock();
            serde_json::to_writer(&mut o, &r)?;
            o.write_all(b"\n")?;
            o.flush()?;
        }
    }
    Ok(())
}
