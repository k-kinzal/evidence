#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use evidence_core::{AssessmentOutcome, Project};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Response, Server};

const SOURCE: &str = "13:42 復旧作業を開始した。\n13:50 復旧した。\n";

fn insufficient() -> Value {
    json!({
        "type": "choice", "choice": "insufficient",
        "probabilities": {"supported": 0.01, "partial": 0.01, "insufficient": 0.97, "contradicted": 0.01}
    })
}

fn workspace(root: &Path) -> Project {
    let project = Project::init(root).unwrap();
    std::fs::write(root.join("log.md"), SOURCE).unwrap();
    std::fs::write(root.join("log.md.evid"), "version = 2\nid = 'log'\n").unwrap();
    std::fs::write(
        root.join("foo.md"),
        "13:42 に復旧した。\n13:50 に復旧した。\n",
    )
    .unwrap();
    std::fs::write(root.join("bar.md"), "13:42 に復旧したと報告された。\n").unwrap();
    std::fs::write(
        root.join("foo.evid"),
        r"
version = 2
id = 'foo'
subject = 'foo.md'

[[claims]]
id = 'start'
select = { lines = '1' }
evidence = [{ ref = 'log', select = { lines = '1' } }]

[[claims]]
id = 'end'
select = { lines = '2' }
evidence = [{ ref = 'log', select = { lines = '2' } }]
",
    )
    .unwrap();
    std::fs::write(
        root.join("bar.md.evid"),
        r"
version = 2
id = 'bar'

[[claims]]
id = 'start'
select = { lines = '1' }
evidence = [{ ref = 'log', select = { lines = '1' } }]

[[claims]]
id = 'other'
select = { lines = '1' }
evidence = [{ ref = 'log', select = { lines = '1' } }]
",
    )
    .unwrap();
    project
}

fn assess(root: &Path, endpoint: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_assess"));
    command
        .current_dir(root)
        .env_clear()
        .env("ASSESS_TEST_KEY", "fixture-key")
        .args(["--endpoint", endpoint, "--timeout", "5", "--json"]);
    command
}

fn server(
    calls: usize,
    respond: impl Fn(&tiny_http::Request, &Value) -> Value + Send + 'static,
) -> (String, JoinHandle<Vec<Value>>) {
    let server = Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/decision", server.server_addr());
    let worker = thread::spawn(move || {
        (0..calls)
            .map(|_| {
                let mut request = server
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                    .expect("request");
                assert_eq!(request.method(), &Method::Post);
                let input = serde_json::from_reader(request.as_reader()).unwrap();
                let body = respond(&request, &input);
                request
                    .respond(
                        Response::from_string(body.to_string())
                            .with_header(
                                Header::from_bytes("Content-Type", "application/json").unwrap(),
                            )
                            .with_header(
                                Header::from_bytes("x-request-id", "request-123").unwrap(),
                            ),
                    )
                    .unwrap();
                input
            })
            .collect()
    });
    (endpoint, worker)
}

fn output_value(output: &Output, exit_code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit_code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn batches_across_evidence_and_replays_the_original_inputs_after_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let mut project = workspace(dir.path());
    let (endpoint, worker) = server(4, |_, input| {
        let mut answers = serde_json::Map::new();
        for name in input["questions"].as_object().unwrap().keys() {
            answers.insert(name.clone(), insufficient());
        }
        json!({ "id": "decision-xyz", "model": "fixed-test-model", "answers": answers })
    });
    let output = assess(dir.path(), &endpoint)
        .args([
            "citation-correctness",
            "--provider",
            "system-one",
            "--model",
            "test-model",
            "foo",
            "bar.md.evid",
        ])
        .output()
        .unwrap();
    let first = output_value(&output, 0);
    let id = first["id"].as_str().unwrap();
    assert_eq!(first["results"].as_array().unwrap().len(), 4);

    project.reload().unwrap();
    let foo = project.find("foo").unwrap();
    let bar = project.find("bar").unwrap();
    let foo_record = project.show(foo).assessments.unwrap();
    let bar_record = project.assessment_file(bar).unwrap().unwrap();
    assert_eq!(foo_record.runs[0].id.to_string(), id);
    assert_eq!(bar_record.runs[0].id.to_string(), id);
    assert_eq!(foo_record.runs[0].results.len(), 2);
    assert_eq!(bar_record.runs[0].results.len(), 2);
    assert!(dir.path().join("foo.asse").is_file());
    assert!(dir.path().join("bar.md.asse").is_file());
    assert!(
        !std::fs::read_to_string(dir.path().join("foo.asse"))
            .unwrap()
            .contains("復旧作業")
    );
    let old = project
        .assessment_archive(foo, id.parse().unwrap())
        .unwrap();
    assert_eq!(old.exchange["requests"][0]["request_id"], "request-123");
    assert_eq!(
        old.exchange["requests"][0]["response_model"],
        "fixed-test-model"
    );

    std::fs::write(dir.path().join("log.md"), "new source\n").unwrap();
    std::fs::write(dir.path().join("foo.md"), "new subject\n").unwrap();
    project.prune_store(true).unwrap();
    let replay = assess(dir.path(), &endpoint)
        .args(["replay", "foo", id])
        .output()
        .unwrap();
    let next = output_value(&replay, 0);
    assert_ne!(next["id"], id);
    assert_eq!(project.assessment_file(foo).unwrap().unwrap().runs.len(), 2);
    assert_eq!(project.assessment_file(bar).unwrap().unwrap().runs.len(), 2);

    let requests = worker.join().unwrap();
    let request_order = |range: std::ops::Range<usize>| {
        let mut sorted = requests[range].to_vec();
        sorted.sort_by_key(|request| request["state"].to_string());
        sorted
    };
    let original = request_order(0..2);
    assert_eq!(original, request_order(2..4));
    let shared = original
        .iter()
        .find(|request| request["questions"].as_object().unwrap().len() == 3)
        .unwrap();
    assert_eq!(
        shared["state"]["citations"][0].as_str().unwrap().trim(),
        "13:42 復旧作業を開始した。"
    );
    let claims = shared["state"]["claims"].as_array().unwrap();
    let claims: Vec<_> = claims
        .iter()
        .map(|claim| claim.as_str().unwrap().trim())
        .collect();
    assert_eq!(
        claims,
        [
            "13:42 に復旧したと報告された。",
            "13:42 に復旧したと報告された。",
            "13:42 に復旧した。",
        ]
    );
    for index in 0..claims.len() {
        let question = &shared["questions"][format!("q{index}")];
        let instructions = question["instructions"].as_str().unwrap();
        assert!(instructions.contains(&format!("`claims[{index}]`")));
        assert!(instructions.contains("`citations`"));
        assert!(!instructions.contains("13:42"));
        assert!(
            question["criteria"]["contradicted"]
                .as_str()
                .unwrap()
                .contains("same subject and conditions")
        );
        assert!(
            question["criteria"]["supported"]
                .as_str()
                .unwrap()
                .contains("No material part is contradicted.")
        );
    }
    assert!(!shared.to_string().contains("13:50"));
}

#[test]
fn openai_matches_answers_by_name_and_records_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let mut project = workspace(dir.path());
    let (endpoint, worker) = server(1, |request, input| {
        assert!(
            request
                .headers()
                .iter()
                .any(|header| header.field.equiv("authorization")
                    && header.value == "Bearer fixture-key")
        );
        let state: Value = serde_json::from_str(input["input"].as_str().unwrap()).unwrap();
        assert!(
            state["citations"][0]
                .as_str()
                .unwrap()
                .contains("13:42 復旧作業")
        );
        assert_eq!(state["claims"].as_array().unwrap().len(), 2);
        assert!(
            input["questions"][1]["instructions"]
                .as_str()
                .unwrap()
                .contains("`claims[1]`")
        );
        assert_eq!(input["questions"][0]["choices"][0]["value"], "supported");
        json!({ "answers": [
            { "name": "q1", "type": "refusal" },
            { "name": "q0", "type": "choice", "choice": "partial", "confidence": 0.8,
              "probabilities": [
                  {"value": "supported", "probability": 0.05},
                  {"value": "partial", "probability": 0.85},
                  {"value": "insufficient", "probability": 0.06},
                  {"value": "contradicted", "probability": 0.04}
              ] }
        ] })
    });
    let output = assess(dir.path(), &endpoint)
        .args([
            "citation-correctness",
            "--provider",
            "openai",
            "--api-key-env",
            "ASSESS_TEST_KEY",
            "bar",
        ])
        .output()
        .unwrap();
    let result = output_value(&output, 1);
    worker.join().unwrap();
    assert_eq!(
        result["results"][0]["value"]["probabilities"]["partial"],
        0.85
    );
    project.reload().unwrap();
    let record = project
        .assessment_file(project.find("bar").unwrap())
        .unwrap()
        .unwrap();
    assert!(matches!(
        record.runs[0].results[1].outcome,
        AssessmentOutcome::Refused
    ));
}

#[test]
fn gemini_and_workers_ai_send_their_native_request_shapes() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());
    for provider in ["gemini", "cloudflare"] {
        let (endpoint, worker) = server(1, move |request, input| {
            let (expected_auth, state) = if provider == "gemini" {
                assert_eq!(input["model"], "gemini-decision-1");
                assert!(input.get("state").is_none());
                let state = serde_json::from_str(input["input"].as_str().unwrap()).unwrap();
                (("x-goog-api-key", "fixture-key"), state)
            } else {
                assert!(input.get("model").is_none());
                (
                    ("authorization", "Bearer fixture-key"),
                    input["state"].clone(),
                )
            };
            assert!(state["citations"][0].as_str().unwrap().contains("復旧作業"));
            assert_eq!(
                state["claims"][0].as_str().unwrap().trim(),
                "13:42 に復旧した。"
            );
            assert!(request.headers().iter().any(|header| {
                header.field.equiv(expected_auth.0) && header.value == expected_auth.1
            }));
            assert!(input["questions"]["q0"]["criteria"]["supported"].is_string());
            let answer = json!({"answers": {"q0": insufficient()}});
            if provider == "cloudflare" {
                json!({"success": true, "result": answer})
            } else {
                answer
            }
        });
        let endpoint = if provider == "cloudflare" {
            format!("{endpoint}/run/@cf/cloudflare/clef")
        } else {
            endpoint
        };
        let output = assess(dir.path(), &endpoint)
            .args([
                "citation-correctness",
                "--provider",
                provider,
                "--api-key-env",
                "ASSESS_TEST_KEY",
                "foo#start",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output_value(&output, 0)["results"][0]["value"]["label"],
            "insufficient"
        );
        worker.join().unwrap();
    }
}

#[test]
fn records_missing_or_invalid_distributions_as_failures_and_keeps_the_response() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());
    let (endpoint, worker) = server(1, |_, _| {
        json!({
            "answers": {
                "q0": {"type": "choice", "choice": "supported"},
                "q1": {"type": "choice", "choice": "supported",
                       "probabilities": {"supported": 0.8, "partial": 0.6, "insufficient": 0.0, "contradicted": 0.0}}
            }
        })
    });
    let output = assess(dir.path(), &endpoint)
        .args([
            "citation-correctness",
            "--provider",
            "system-one",
            "--model",
            "test-model",
            "bar",
        ])
        .output()
        .unwrap();
    let run = output_value(&output, 1);
    worker.join().unwrap();
    for (index, expected) in ["omitted", "invalid"].into_iter().enumerate() {
        let result = &run["results"][index];
        assert_eq!(result["status"], "failed");
        assert!(result["error"].as_str().unwrap().contains(expected));
    }
    let project = Project::open(dir.path()).unwrap();
    let archive = project
        .assessment_archive(
            project.find("bar").unwrap(),
            run["id"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
    assert_eq!(
        archive.exchange["requests"][0]["response"]["answers"]["q1"]["probabilities"]["supported"],
        0.8
    );
}
