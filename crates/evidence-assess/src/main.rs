mod adapter;
mod coverage;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum as _, value_parser};
use evidence_core::format::ObjectId;
use evidence_core::{
    AssessmentMethod, AssessmentOutcome, AssessmentResult, AssessmentTarget, CapturedClaim, Project,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use adapter::{Adapter, Connection, Exchange, Provider};

const CITATION_CORRECTNESS: &str = "citation-correctness";

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Measure Evidence coverage and assess citation support"
)]
struct Args {
    /// Directory in an Evidence project.
    #[arg(short = 'C', long, default_value = ".", global = true)]
    directory: PathBuf,
    /// Decision API to use (default: jev for new evaluations).
    #[arg(long, env = "ASSESS_PROVIDER", global = true)]
    provider: Option<Provider>,
    /// Full Decision API URL, including the operation path.
    #[arg(long, env = "ASSESS_ENDPOINT", global = true)]
    endpoint: Option<String>,
    /// Model ID or provider deployment name.
    #[arg(long, env = "ASSESS_MODEL", global = true)]
    model: Option<String>,
    /// Read the credential from this environment variable instead of the provider default.
    #[arg(long, env = "ASSESS_API_KEY_ENV", global = true)]
    api_key_env: Option<String>,
    /// Maximum number of independent requests in flight.
    #[arg(long, default_value_t = 4, value_parser = value_parser!(u16).range(1..=64), global = true)]
    jobs: u16,
    /// Deadline in seconds for each HTTP request.
    #[arg(long, default_value_t = 120, value_parser = value_parser!(u32).range(1..), global = true)]
    timeout: u32,
    /// Print the run ID and results as structured output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report claim, usage, or requirement coverage.
    Coverage(coverage::CoverageArgs),
    /// Assess how well each claim is supported by its citations.
    CitationCorrectness {
        /// Evidence IDs or paths, optionally restricted to a claim with `#claim`.
        #[arg(required = true)]
        targets: Vec<String>,
        /// Maximum number of questions sharing the same citations in one request.
        #[arg(long, default_value_t = 16, value_parser = value_parser!(u16).range(1..=64))]
        questions_per_request: u16,
    },
    /// Resend the original requests for an entire recorded run.
    Replay {
        /// Any Evidence ID or path linked to that run.
        target: String,
        /// Run ID from the assessment record.
        run: ObjectId,
    },
}

struct Plan {
    requests: Vec<Value>,
    bindings: Vec<Binding>,
    replay_of: Option<ObjectId>,
}

struct Binding {
    target: AssessmentTarget,
    request: usize,
    question: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunExchange {
    assess_version: String,
    connection: Connection,
    requests: Vec<Exchange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replay_of: Option<ObjectId>,
}

impl Args {
    fn connection(&self) -> Result<Connection> {
        let provider = self.provider.unwrap_or(Provider::Jev);
        let endpoint = self
            .endpoint
            .clone()
            .or_else(|| provider.endpoint().map(str::to_owned))
            .context("this provider requires --endpoint")?;
        let model = if provider == Provider::Cloudflare {
            let model = endpoint
                .rsplit_once("/run/")
                .map(|(_, model)| model)
                .filter(|model| !model.is_empty())
                .context("the Workers AI endpoint must include /run/<model>")?;
            ensure!(
                self.model
                    .as_deref()
                    .is_none_or(|expected| expected == model),
                "--model does not match the Workers AI endpoint"
            );
            model.to_owned()
        } else {
            self.model
                .clone()
                .or_else(|| provider.model().map(str::to_owned))
                .context("this provider requires --model")?
        };
        Ok(Connection {
            provider,
            endpoint,
            model,
        })
    }

    fn adapter(&self, connection: Connection) -> Result<Adapter> {
        Adapter::connect(
            connection,
            self.api_key_env.as_deref(),
            Duration::from_secs(u64::from(self.timeout)),
        )
    }
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "evidence-assess: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> Result<bool> {
    let mut project = Project::open(&args.directory)?;
    let (adapter, plan) = match &args.command {
        Command::CitationCorrectness {
            targets,
            questions_per_request,
        } => {
            let adapter = args.adapter(args.connection()?)?;
            let targets = select(&project, targets)?;
            let captures = project.capture_assessment_inputs(&targets)?;
            let plan = prepare(&adapter, captures, usize::from(*questions_per_request));
            (adapter, plan)
        }
        Command::Replay { target, run } => replay(args, &project, target, *run)?,
        Command::Coverage(coverage) => {
            return Ok(self::coverage::run(&project, coverage, args.json)? == 0);
        }
    };

    let exchanges = send_requests(&adapter, &plan.requests, usize::from(args.jobs));
    let mut results = Vec::with_capacity(plan.bindings.len());
    for binding in plan.bindings {
        let exchange = exchanges
            .get(binding.request)
            .context("missing exchange for an assessment target")?;
        let outcome = adapter.outcome(exchange, &binding.question);
        results.push(AssessmentResult {
            target: binding.target,
            request: binding.request,
            question: binding.question,
            outcome,
        });
    }
    let exchange = RunExchange {
        assess_version: env!("CARGO_PKG_VERSION").into(),
        connection: adapter.connection,
        requests: exchanges,
        replay_of: plan.replay_of,
    };
    let method = AssessmentMethod {
        kind: CITATION_CORRECTNESS.into(),
        evaluator: exchange
            .connection
            .provider
            .to_possible_value()
            .context("unknown provider name")?
            .get_name()
            .to_owned(),
        model: Some(exchange.connection.model.clone()),
    };
    let id = project.record_assessment(method, serde_json::to_value(exchange)?, results.clone())?;
    print_run(id, &results, args.json)?;
    Ok(results
        .iter()
        .all(|result| matches!(result.outcome, AssessmentOutcome::Assessed { .. })))
}

fn select(project: &Project, targets: &[String]) -> Result<Vec<(String, String)>> {
    let mut claims = BTreeSet::new();
    for target in targets {
        if let Some((pair, claim)) = target.split_once('#') {
            let pair = project.find(pair)?;
            ensure!(
                pair.file.claim(claim).is_some(),
                "unknown claim {}#{claim}",
                pair.id
            );
            claims.insert((pair.id.clone(), claim.to_owned()));
        } else {
            for pair in project.select(std::slice::from_ref(target))? {
                claims.extend(
                    pair.file
                        .all_claims()
                        .into_iter()
                        .map(|(claim, _)| (pair.id.clone(), claim.id.clone())),
                );
            }
        }
    }
    ensure!(!claims.is_empty(), "the selected evidence has no claims");
    Ok(claims.into_iter().collect())
}

fn prepare(adapter: &Adapter, captures: Vec<CapturedClaim>, batch_size: usize) -> Plan {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for capture in captures {
        groups
            .entry(capture.citations)
            .or_default()
            .push((capture.target, capture.statement));
    }
    let mut requests = Vec::new();
    let mut bindings = Vec::new();
    for (citations, group) in groups {
        for chunk in group.chunks(batch_size) {
            let request = requests.len();
            let statements: Vec<_> = chunk
                .iter()
                .map(|(_, statement)| statement.clone())
                .collect();
            requests.push(adapter.request(&citations, &statements));
            for (index, (target, _)) in chunk.iter().enumerate() {
                bindings.push(Binding {
                    target: target.clone(),
                    request,
                    question: format!("q{index}"),
                });
            }
        }
    }
    Plan {
        requests,
        bindings,
        replay_of: None,
    }
}

fn replay(args: &Args, project: &Project, target: &str, id: ObjectId) -> Result<(Adapter, Plan)> {
    let old = project.assessment_archive(project.find(target)?, id)?;
    ensure!(
        old.method.kind == CITATION_CORRECTNESS,
        "unsupported assessment kind {:?}",
        old.method.kind
    );
    let exchange: RunExchange = serde_json::from_value(old.exchange)
        .context("cannot read the original Decision API run")?;
    let connection = exchange.connection;
    ensure!(
        args.provider
            .is_none_or(|provider| provider == connection.provider),
        "--provider must match the recorded run"
    );
    ensure!(
        args.model
            .as_ref()
            .is_none_or(|model| *model == connection.model),
        "--model must match the recorded run"
    );
    // Replay data is project input. Credentials may only be used at the configured endpoint.
    ensure!(
        args.endpoint
            .as_deref()
            .or_else(|| connection.provider.endpoint())
            == Some(&connection.endpoint),
        "set --endpoint to the original endpoint ({}) to replay this run",
        connection.endpoint
    );
    let requests: Vec<_> = exchange
        .requests
        .into_iter()
        .map(|entry| entry.request)
        .collect();
    ensure!(
        !old.results.is_empty()
            && old
                .results
                .iter()
                .all(|result| result.request < requests.len()),
        "invalid request references in the original run"
    );
    let bindings = old
        .results
        .into_iter()
        .map(|result| Binding {
            target: result.target,
            request: result.request,
            question: result.question,
        })
        .collect();
    let plan = Plan {
        requests,
        bindings,
        replay_of: Some(id),
    };
    Ok((args.adapter(connection)?, plan))
}

fn send_requests(adapter: &Adapter, requests: &[Value], jobs: usize) -> Vec<Exchange> {
    let next = AtomicUsize::new(0);
    let mut indexed = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..jobs.min(requests.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut results = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(request) = requests.get(index) else {
                            break;
                        };
                        results.push((index, adapter.send(request.clone())));
                    }
                    results
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| match worker.join() {
                Ok(values) => values,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect::<Vec<_>>()
    });
    indexed.sort_unstable_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, exchange)| exchange).collect()
}

fn print_run(id: ObjectId, results: &[AssessmentResult], json_output: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if json_output {
        serde_json::to_writer_pretty(&mut stdout, &json!({ "id": id, "results": results }))?;
        writeln!(stdout)?;
        return Ok(());
    }
    writeln!(stdout, "Assessment {id}")?;
    for result in results {
        let answer = match &result.outcome {
            AssessmentOutcome::Assessed { value } => {
                let label = value["label"].as_str().unwrap_or("assessed");
                format!(
                    "{label}  P(supported)={}",
                    value["probabilities"]["supported"]
                )
            }
            AssessmentOutcome::Refused => "refused".into(),
            AssessmentOutcome::Failed { error } => format!("failed ({error})"),
        };
        writeln!(
            stdout,
            "{}#{}  {}",
            result.target.statement.evidence, result.target.claim, answer
        )?;
    }
    Ok(())
}

/// Prints a line to stdout (ignoring broken pipes).
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}

/// Prints a line to stderr.
#[macro_export]
macro_rules! warn_line {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr().lock(), $($arg)*);
    }};
}
