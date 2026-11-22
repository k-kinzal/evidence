//! Converting a version 1 definition and state to version 2 (`docs/migration.md`).
//!
//! The conversion never strengthens history: records without a fingerprint stay
//! unverified (`legacy`), a fixed reference without a recorded revision stays unknown, and
//! nothing a generator pinned is assumed to be generated.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::{Map, Value};
use span_select::Selector;

use super::evid::{Action as V1Action, EvidenceFile as V1File, InputRef, Kind};
use super::evid::{Lifecycle, Origin};
use super::state::State as V1State;
use crate::format::diagnostic::Diagnostic;
use crate::format::evid::{
    Action, Claim, EvidenceFile, FORMAT_VERSION, Output, Primary, Production, Reference,
    ReferenceDetail,
};
use crate::format::state::{
    ContentRecord, FailedAttempt, InputVersion, PinRecord, PinnedContent, PinnedReference,
    RunRecord, RunStatus, State,
};

/// Result of converting a version 1 file.
#[derive(Debug, Clone)]
pub struct Converted {
    /// The version 2 file (definition and state).
    pub file: EvidenceFile,
    /// Problems: errors block a migration (`revision-unknown`); warnings explain what
    /// changed (`generator-claims`).
    pub issues: Vec<Diagnostic>,
}

impl Converted {
    /// Whether the conversion is incomplete and must not be written.
    pub fn blocked(&self) -> bool {
        self.issues.iter().any(Diagnostic::is_error)
    }
}

/// The name the production of a version 1 kind gets.
pub(crate) fn production_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Source => "fetch",
        Kind::Derived => "transform",
        Kind::Authored => "generate",
    }
}

fn production(name: &str, action: &V1Action, inputs: Vec<Reference>) -> Production {
    Production {
        name: name.to_owned(),
        writes: vec![Output::Subject],
        namespace: None,
        run: action.run.clone(),
        stdout: action.stdout,
        stdin: action.stdin.clone(),
        env: action.env.clone(),
        cwd: action.cwd.clone(),
        timeout: action.timeout,
        interval: action.interval,
        inputs,
        locator: action.locator.clone(),
    }
}

fn input(r: &InputRef) -> Reference {
    match r {
        InputRef::Id(id) => Reference::Id(id.clone()),
        InputRef::Detailed(d) => Reference::Detailed(ReferenceDetail {
            reference: d.reference.clone(),
            alias: d.alias.clone(),
            ..ReferenceDetail::default()
        }),
    }
}

/// Converts a version 1 file. See `docs/migration.md` for the rules.
pub fn to_v2(f: &V1File) -> Converted {
    let mut issues = Vec::new();
    let v1_state = f.state();
    let mut file = EvidenceFile {
        version: FORMAT_VERSION,
        id: f.id.clone(),
        title: f.title.clone(),
        description: f.description.clone(),
        tags: f.tags.clone(),
        author: f.author.clone(),
        subject: f.subject.clone(),
        format: f.format,
        productions: Vec::new(),
        verify: f.verify.as_ref().map(|v| Action {
            run: v.run.clone(),
            stdout: v.stdout,
            stdin: v.stdin.clone(),
            env: v.env.clone(),
            cwd: v.cwd.clone(),
            timeout: v.timeout,
            locator: v.locator.clone(),
        }),
        schema: f.schema.clone(),
        claims: Vec::new(),
        primary: Vec::new(),
        requirements: Vec::new(),
        scope: Vec::new(),
        wiki: None,
        members: f.members.clone().map(|mut m| {
            match template_to_v2(&m.template) {
                Ok(t) => m.template = t,
                Err(e) => issues
                    .push(Diagnostic::error("migrate-template", e).at_pointer("/members/template")),
            }
            m
        }),
        state: None,
    };

    let name = production_name(f.kind);
    match f.kind {
        Kind::Source => {
            if let Some(a) = &f.source {
                file.productions.push(production(name, a, Vec::new()));
            }
        }
        Kind::Derived => {
            if let Some(a) = &f.transform {
                file.productions
                    .push(production(name, a, f.inputs.iter().map(input).collect()));
            }
        }
        Kind::Authored => {
            if let Some(a) = &f.generator {
                // Version 1 handed a generator its claims' references as inputs, too.
                let mut inputs: Vec<Reference> = f.inputs.iter().map(input).collect();
                for dep in f.dependencies() {
                    if !inputs.iter().any(|i| i.id() == dep) {
                        inputs.push(Reference::Id(dep.to_owned()));
                    }
                }
                file.productions.push(production(name, a, inputs));
                if f.claims.iter().any(|c| c.origin.is_none()) {
                    issues.push(
                        Diagnostic::warning(
                            "generator-claims",
                            "version 1 re-pinned every claim after a generator run; in version \
                             2 these claims are hand-written and are pinned with `evidence \
                             update --pin`. To let the generator write claims, add `claims` to \
                             its `writes` and give it a `namespace`.",
                        )
                        .at_pointer("/generator"),
                    );
                }
            }
        }
    }

    let fixed = f.lifecycle() == Lifecycle::Snapshot;
    // Old reference key -> new reference key, for pins.
    let mut keys: BTreeMap<(String, String), String> = BTreeMap::new();
    for (i, c) in f.claims.iter().enumerate() {
        if c.origin == Some(Origin::Primary) {
            file.primary.push(Primary {
                select: c.select.clone(),
                note: c.note.clone(),
            });
            continue;
        }
        let pin = v1_state.claims.get(&c.id);
        let mut evidence = Vec::new();
        for (j, r) in c.evidence.iter().enumerate() {
            let old_key = r.key();
            let mut detail = ReferenceDetail {
                reference: r.id().to_owned(),
                select: r.selector().cloned(),
                path: r.path().map(str::to_owned),
                ..ReferenceDetail::default()
            };
            if fixed {
                if let Some(pinned) = pin.and_then(|p| p.evidence.iter().find(|e| e.key == old_key))
                {
                    detail.revision = Some(pinned.snapshot);
                } else {
                    {
                        detail.revision_unknown = true;
                        issues.push(
                            Diagnostic::error(
                                "revision-unknown",
                                format!(
                                    "claim `{}`: `lifecycle = \"snapshot\"` fixes `{old_key}`, \
                                     but no pin recorded its revision; write the revision to \
                                     fix (`revision = \"sha256:…\"`) or remove the lifecycle \
                                     before migrating",
                                    c.id
                                ),
                            )
                            .at_pointer(format!("/claims/{i}/evidence/{j}")),
                        );
                    }
                }
            }
            let new = if detail
                == (ReferenceDetail {
                    reference: r.id().to_owned(),
                    ..ReferenceDetail::default()
                }) {
                Reference::Id(r.id().to_owned())
            } else {
                Reference::Detailed(detail)
            };
            keys.insert((c.id.clone(), old_key), new.key());
            evidence.push(new);
        }
        file.claims.push(Claim {
            id: c.id.clone(),
            select: c.select.clone(),
            evidence,
            note: c.note.clone(),
            broad: None,
        });
    }
    if f.origin == Some(Origin::Primary) {
        file.primary.push(Primary {
            select: Selector::Whole(true),
            note: None,
        });
    }
    let by_generator: Vec<&str> = v1_state
        .claims
        .iter()
        .filter(|(_, s)| {
            s.reviewed_by
                .as_deref()
                .is_some_and(|b| b.starts_with("generator:"))
        })
        .map(|(id, _)| id.as_str())
        .collect();
    if !by_generator.is_empty() {
        issues.push(Diagnostic::warning(
            "generator-pins",
            format!(
                "pins of {} were recorded by a generator in version 1; they are kept as \
                 unverified pins of hand-written claims, not as generated claims",
                by_generator.join(", ")
            ),
        ));
    }

    let state = convert_state(f, &v1_state, name, &keys);
    file.state = (!state.is_empty()).then_some(state);
    Converted { file, issues }
}

fn convert_state(
    f: &V1File,
    s: &V1State,
    name: &str,
    keys: &BTreeMap<(String, String), String>,
) -> State {
    let mut state = State {
        member: s.member.clone(),
        ..State::default()
    };
    let produced = f.producer().is_some();
    if produced {
        if let Some(snapshot) = s.snapshot {
            let changed = s
                .updated_at
                .or(s.checked_at)
                .unwrap_or(Timestamp::UNIX_EPOCH);
            state.content = Some(ContentRecord {
                snapshot,
                updated_at: changed,
                production: Some(name.to_owned()),
                run: None,
            });
            let inputs = s
                .inputs
                .iter()
                .map(|(id, snap)| {
                    let input_name = f
                        .inputs
                        .iter()
                        .find(|i| i.id() == id)
                        .map_or(id.as_str(), InputRef::name);
                    (
                        input_name.to_owned(),
                        InputVersion {
                            id: id.clone(),
                            snapshot: *snap,
                            fragment: None,
                            fixed: false,
                        },
                    )
                })
                .collect();
            state.runs.insert(
                name.to_owned(),
                RunRecord {
                    run: None,
                    at: s.checked_at.unwrap_or(changed),
                    fingerprint: None,
                    fingerprint_version: None,
                    subject: Some(snapshot),
                    inputs,
                    claims: None,
                    namespace: None,
                    locator: s.locator.clone(),
                    legacy: true,
                },
            );
        }
        if let Some(last) = &s.last_run
            && last.status == RunStatus::Failed
        {
            state.failures.insert(
                name.to_owned(),
                FailedAttempt {
                    at: last.at,
                    run: None,
                    message: last.message.clone().unwrap_or_else(|| "failed".into()),
                },
            );
        }
    }
    for (id, cs) in &s.claims {
        let Some(claim) = f.claims.iter().find(|c| &c.id == id) else {
            continue;
        };
        if claim.origin == Some(Origin::Primary) {
            continue;
        }
        state.pins.insert(
            id.clone(),
            PinRecord {
                recorded_at: cs.reviewed_at,
                recorded_by: cs.reviewed_by.clone(),
                run: None,
                definition: None,
                fingerprint_version: None,
                content: PinnedContent {
                    snapshot: None,
                    fragment: cs.content,
                },
                anchor: cs.anchor.clone(),
                evidence: cs
                    .evidence
                    .iter()
                    .map(|p| PinnedReference {
                        key: keys
                            .get(&(id.clone(), p.key.clone()))
                            .cloned()
                            .unwrap_or_else(|| p.key.clone()),
                        snapshot: p.snapshot,
                        fragment: p.fragment,
                    })
                    .collect(),
                legacy: true,
            },
        );
    }
    state
}

/// Converts a version 1 `[members]` template (any evidence file fields except `version`,
/// `id`, `subject`, `members`, `state`; `kind` defaults to `source`) to version 2 fields.
pub fn template_to_v2(template: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut doc = template.clone();
    doc.entry("kind").or_insert_with(|| "source".into());
    doc.insert("version".into(), 1.into());
    doc.insert("id".into(), "template".into());
    let v1: V1File = serde_json::from_value(Value::Object(doc))
        .map_err(|e| format!("the version 1 template cannot be read: {e}"))?;
    let converted = to_v2(&v1);
    if let Some(problem) = converted.issues.iter().find(|d| d.is_error()) {
        return Err(problem.message.clone());
    }
    let Value::Object(mut out) =
        serde_json::to_value(&converted.file).map_err(|e| e.to_string())?
    else {
        return Err("internal: template is not a table".into());
    };
    for k in ["version", "id", "state"] {
        out.remove(k);
    }
    Ok(out)
}
