//! Versioned claim inputs and `.asse` records produced by external evaluators.

use std::collections::{BTreeMap, HashMap};

use cas_fs::{ObjectId, Storage};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::format::{RefVersion, Selector, SnapshotStorage};
use crate::journal::{self, Transaction};
use crate::layout::Layout;
use crate::snapshot::{Resolver, Version};
use crate::workspace::{Pair, Workspace};

const VERSION: u32 = 1;

/// The exact selection used in an assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentInput {
    /// Evidence ID of the selected subject.
    pub evidence: String,
    /// Snapshot of the subject containing the selection.
    pub snapshot: ObjectId,
    /// Storage of the snapshot, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<SnapshotStorage>,
    /// File selected inside a directory subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Range of the file supplied to the evaluator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<Selector>,
    /// Hash of the exact text supplied, including whitespace.
    pub fragment: ObjectId,
}

/// A claim and the versions of its statement and citations supplied to the evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentTarget {
    /// Claim ID within `statement.evidence`.
    pub claim: String,
    /// Fingerprint of the claim definition used.
    pub definition: ObjectId,
    /// Subject and selected claim text.
    pub statement: AssessmentInput,
    /// Selected citation text, in claim order.
    pub citations: Vec<AssessmentInput>,
}

/// Text supplied to an external evaluator, with the versions it came from.
#[derive(Debug, Clone)]
pub struct CapturedClaim {
    /// Claim and version identifiers.
    pub target: AssessmentTarget,
    /// Exact selected claim text.
    pub statement: String,
    /// Exact selected source texts, in citation order.
    pub citations: Vec<String>,
}

/// The outcome of asking an evaluator about a claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AssessmentOutcome {
    /// The evaluator supplied a value.
    Assessed {
        /// Value in the evaluator's chosen format.
        value: Value,
    },
    /// The evaluator declined to answer.
    Refused,
    /// No usable answer could be obtained.
    Failed {
        /// Description of the failure.
        error: String,
    },
}

/// A single result and its location in the external run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssessmentResult {
    /// Assessed claim and its inputs.
    pub target: AssessmentTarget,
    /// Zero-based position of the request in the external record.
    pub request: usize,
    /// Name of the question in that request.
    pub question: String,
    /// Value or execution status.
    #[serde(flatten)]
    pub outcome: AssessmentOutcome,
}

/// Evaluator and type of assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentMethod {
    /// What was assessed.
    pub kind: String,
    /// Name of the external evaluator or service.
    pub evaluator: String,
    /// Model or deployment requested by the evaluator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A recorded run, restricted to the results for one Evidence subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssessmentRun {
    /// Content address of the complete run, shared by its sidecars.
    pub id: ObjectId,
    /// Storage class of the archive.
    pub storage: SnapshotStorage,
    /// When the run was recorded.
    pub at: Timestamp,
    /// Evaluator and assessment kind.
    #[serde(flatten)]
    pub method: AssessmentMethod,
    /// Results for this Evidence subject.
    pub results: Vec<AssessmentResult>,
}

/// Evaluation history in a `.asse` sidecar next to an Evidence definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentFile {
    /// Format version.
    pub version: u32,
    /// Evidence ID the record belongs to.
    pub evidence: String,
    /// Runs in publication order.
    pub runs: Vec<AssessmentRun>,
}

/// The complete record of an external run, stored under the project storage policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssessmentArchive {
    /// Archive format version.
    pub version: u32,
    /// When the run was recorded.
    pub at: Timestamp,
    /// Evaluator and assessment kind.
    #[serde(flatten)]
    pub method: AssessmentMethod,
    /// All of the run's results, including results linked from other sidecars.
    pub results: Vec<AssessmentResult>,
    /// Opaque evaluator metadata and the original requests and responses.
    pub exchange: Value,
}

impl AssessmentFile {
    pub(crate) fn path(pair: &Pair) -> std::path::PathBuf {
        pair.evid_path.with_extension("asse")
    }

    pub(crate) fn read(pair: &Pair) -> Result<Option<Self>> {
        let path = Self::path(pair);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "{} is not a regular file",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(&path, e)),
        }
        let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
        let record: Self = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
        if record.version != VERSION || record.evidence != pair.id {
            return Err(Error::Invalid(format!(
                "{} has an unsupported version or belongs to a different Evidence ID",
                path.display()
            )));
        }
        Ok(Some(record))
    }
}

pub(crate) fn capture(layout: &Layout, targets: &[(String, String)]) -> Result<Vec<CapturedClaim>> {
    let _lock = layout.lock()?;
    let layout = &layout.writable()?;
    journal::recover(layout)?;
    let ws = Workspace::load(layout)?;
    let resolver = Resolver::new(&ws);
    let mut versions = HashMap::new();
    let mut version = |evidence: &str, revision: RefVersion| -> Result<Version> {
        let key = (evidence.to_owned(), revision);
        if let Some(saved) = versions.get(&key) {
            return Ok(Version::clone(saved));
        }
        let pair = ws.get(evidence);
        let resolved = match revision {
            RefVersion::Current => {
                resolver.current(pair.ok_or_else(|| Error::NotFound(evidence.to_owned()))?)
            }
            RefVersion::Revision(id) => resolver.fixed(pair, id),
        }
        .map_err(|e| Error::Invalid(format!("{evidence}: {e}")))?;
        let frozen = resolver.keep(&resolved)?;
        versions.insert(key, frozen.clone());
        Ok(frozen)
    };

    let mut captures = Vec::with_capacity(targets.len());
    for (id, claim_id) in targets {
        let pair = ws.get(id).ok_or_else(|| Error::NotFound(id.clone()))?;
        check_path(&ws, pair)?;
        AssessmentFile::read(pair)?;
        let (claim, _) = pair
            .file
            .claim(claim_id)
            .ok_or_else(|| Error::NotFound(format!("{id}#{claim_id}")))?;
        let subject = version(id, RefVersion::Current)?;
        let (statement, statement_text) =
            excerpt(resolver, id, &subject, None, Some(&claim.select))?;
        let mut citations = Vec::with_capacity(claim.evidence.len());
        let mut citation_texts = Vec::with_capacity(claim.evidence.len());
        for reference in &claim.evidence {
            let source = version(reference.id(), reference.version())?;
            let (input, text) = excerpt(
                resolver,
                reference.id(),
                &source,
                reference.path(),
                reference.selector(),
            )?;
            citations.push(input);
            citation_texts.push(text);
        }
        captures.push(CapturedClaim {
            target: AssessmentTarget {
                claim: claim.id.clone(),
                definition: crate::fingerprint::claim(claim),
                statement,
                citations,
            },
            statement: statement_text,
            citations: citation_texts,
        });
    }
    Ok(captures)
}

fn excerpt(
    resolver: Resolver<'_>,
    evidence: &str,
    version: &Version,
    path: Option<&str>,
    select: Option<&Selector>,
) -> Result<(AssessmentInput, String)> {
    let text = resolver
        .text(version, path)
        .map_err(|e| Error::Invalid(format!("{evidence}: {e}")))?;
    let text = match select {
        Some(selector) => selector
            .extract(&text)
            .map_err(|e| Error::Invalid(format!("{evidence}: {e}")))?
            .to_owned(),
        None => text,
    };
    Ok((
        AssessmentInput {
            evidence: evidence.to_owned(),
            snapshot: version.snapshot(),
            storage: version.storage(),
            path: path.map(str::to_owned),
            select: select.cloned(),
            fragment: ObjectId::of(text.as_bytes()),
        },
        text,
    ))
}

fn check_path(ws: &Workspace, pair: &Pair) -> Result<()> {
    let path = AssessmentFile::path(pair);
    if ws.pairs().iter().any(|other| other.subject_path == path) {
        return Err(Error::Invalid(format!(
            "{} is already an Evidence subject",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn record(
    layout: &Layout,
    at: Timestamp,
    method: AssessmentMethod,
    exchange: Value,
    results: Vec<AssessmentResult>,
) -> Result<ObjectId> {
    if results.is_empty() {
        return Err(Error::Invalid(
            "an assessment must have at least one result".into(),
        ));
    }
    let _lock = layout.lock()?;
    let layout = &layout.writable()?;
    journal::recover(layout)?;
    let ws = Workspace::load(layout)?;
    let store = ws.store();
    let storage = if layout.config().config.store.encryption_enabled() {
        SnapshotStorage::Encrypted
    } else {
        SnapshotStorage::Plain
    };
    let stored = |id: &ObjectId, class: Option<SnapshotStorage>| match class {
        Some(class) => store.scoped(class.into()).contains(id),
        None => store.contains(id),
    };
    let mut per_pair = BTreeMap::<_, Vec<_>>::new();
    for result in &results {
        let target = &result.target;
        for input in std::iter::once(&target.statement).chain(&target.citations) {
            if !stored(&input.snapshot, input.storage) {
                return Err(Error::Conflict(format!(
                    "{}: snapshot {} is no longer available",
                    input.evidence, input.snapshot
                )));
            }
        }
        per_pair
            .entry(target.statement.evidence.clone())
            .or_default()
            .push(result.clone());
    }
    let archive = AssessmentArchive {
        version: VERSION,
        at,
        method,
        results,
        exchange,
    };
    let bytes = serde_json::to_vec(&archive).map_err(|e| Error::Invalid(e.to_string()))?;
    let id = ObjectId::of(&bytes);
    let mut files = Vec::with_capacity(per_pair.len());
    for (evidence, results) in per_pair {
        let pair = ws
            .get(&evidence)
            .ok_or_else(|| Error::NotFound(evidence.clone()))?;
        check_path(&ws, pair)?;
        let mut sidecar = AssessmentFile::read(pair)?.unwrap_or(AssessmentFile {
            version: VERSION,
            evidence,
            runs: Vec::new(),
        });
        if sidecar.runs.iter().any(|run| run.id == id) {
            continue;
        }
        sidecar.runs.push(AssessmentRun {
            id,
            storage,
            at,
            method: archive.method.clone(),
            results,
        });
        let mut encoded =
            serde_json::to_vec_pretty(&sidecar).map_err(|e| Error::Invalid(e.to_string()))?;
        encoded.push(b'\n');
        files.push((AssessmentFile::path(pair), encoded));
    }
    store.scoped(storage.into()).put_bytes(&bytes)?;
    if !files.is_empty() {
        let mut tx = Transaction::begin(layout, "assess")?;
        for (path, bytes) in files {
            if let Err(e) = tx.stage_bytes(&path, &bytes) {
                tx.abort();
                return Err(e);
            }
        }
        tx.commit()?;
    }
    Ok(id)
}

pub(crate) fn read_archive(ws: &Workspace, pair: &Pair, id: ObjectId) -> Result<AssessmentArchive> {
    let file = AssessmentFile::read(pair)?;
    let run = file
        .as_ref()
        .and_then(|file| file.runs.iter().find(|run| run.id == id))
        .ok_or_else(|| Error::NotFound(format!("{} assessment {id}", pair.id)))?;
    let bytes = ws.store().scoped(run.storage.into()).read_blob(&id)?;
    if ObjectId::of(&bytes) != id {
        return Err(Error::Invalid(format!(
            "assessment archive {id} does not match its digest"
        )));
    }
    let archive: AssessmentArchive = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Invalid(format!("assessment archive {id}: {e}")))?;
    if archive.version != VERSION {
        return Err(Error::Invalid(format!(
            "unsupported assessment archive version in {id}"
        )));
    }
    if archive.at != run.at
        || archive.method != run.method
        || run.results.is_empty()
        || !archive
            .results
            .iter()
            .filter(|result| result.target.statement.evidence == pair.id)
            .eq(&run.results)
    {
        return Err(Error::Invalid(format!(
            "{}: assessment sidecar does not match archive {id}",
            pair.id
        )));
    }
    Ok(archive)
}

pub(crate) fn roots(pair: &Pair) -> Result<Vec<(ObjectId, Option<Storage>)>> {
    let mut roots = Vec::new();
    if let Some(file) = AssessmentFile::read(pair)? {
        for run in file.runs {
            roots.push((run.id, Some(run.storage.into())));
            for result in run.results {
                let target = result.target;
                roots.extend(
                    std::iter::once(target.statement)
                        .chain(target.citations)
                        .map(|input| (input.snapshot, input.storage.map(Into::into))),
                );
            }
        }
    }
    Ok(roots)
}
