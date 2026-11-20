//! `evidence add`: scaffolding a new evidence file.

use std::path::{Path, PathBuf};

use crate::format::document::{self, valid_id};
use crate::format::{Kind, Lifecycle};

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::workspace::{Workspace, inside, normalize};

/// Options of [`add`].
#[derive(Debug, Clone)]
pub struct AddOptions {
    /// Subject path (absolute, or relative to the directory the project was opened from).
    pub subject: PathBuf,
    /// Where to write the evidence file. Defaults to `<subject>.evid` next to the subject;
    /// elsewhere, the file records the subject's location in `subject`.
    pub evid: Option<PathBuf>,
    /// Kind.
    pub kind: Kind,
    /// Id (defaults to one derived from the file name).
    pub id: Option<String>,
    /// Title.
    pub title: Option<String>,
    /// Command (program and arguments) for `[source]`, `[transform]`, or an authored
    /// `[generator]`.
    pub run: Vec<String>,
    /// Capture the command's standard output as the subject.
    pub stdout: bool,
    /// Inputs (derived).
    pub inputs: Vec<String>,
    /// Refresh interval (source).
    pub interval: Option<String>,
    /// Lifecycle (authored/derived).
    pub lifecycle: Option<Lifecycle>,
    /// Tags.
    pub tags: Vec<String>,
}

/// Creates a new evidence file and returns its path. The result is validated before writing.
pub(crate) fn add(layout: &Layout, cwd: &Path, opts: &AddOptions) -> Result<PathBuf> {
    let subject = normalize(&cwd.join(&opts.subject));
    let evid = match &opts.evid {
        Some(path) => normalize(&cwd.join(path)),
        None => document::evid_path(&subject),
    };
    if !evid
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(document::EVID_SUFFIX) && n != document::EVID_SUFFIX)
    {
        return Err(Error::Invalid(format!(
            "{}: evidence files are named `<name>.evid`",
            evid.display()
        )));
    }
    if evid.exists() {
        return Err(Error::Invalid(format!("{} already exists", evid.display())));
    }
    // Evidence files are found by scanning the project, so they must be inside it. The subject
    // may live elsewhere (with a warning).
    if !inside(layout.root(), &evid) {
        return Err(Error::Invalid(format!(
            "{} is outside the project {}; use --evid to put the evidence file inside it",
            evid.display(),
            layout.root().display()
        )));
    }
    let declared_subject = (document::subject_path(&evid).as_ref() != Some(&subject))
        .then(|| subject_entry(layout, &evid, &subject));
    let id = match &opts.id {
        Some(id) => id.clone(),
        None => derive_id(&subject),
    };
    if !valid_id(&id) {
        return Err(Error::Invalid(format!("invalid id `{id}`")));
    }
    let ws = Workspace::load(layout)?;
    if let Some(existing) = ws.get(&id) {
        return Err(Error::Invalid(format!(
            "id `{id}` is already used by {}",
            existing.evid_rel
        )));
    }

    let q = |s: &str| toml_edit::Value::from(s).to_string();
    let mut text = String::new();
    text.push_str("version = 1\n");
    text.push_str(&format!("kind = \"{}\"\n", opts.kind));
    text.push_str(&format!("id = {}\n", q(&id)));
    if let Some(t) = &opts.title {
        text.push_str(&format!("title = {}\n", q(t)));
    }
    if let Some(s) = &declared_subject {
        text.push_str(&format!("subject = {}\n", q(s)));
    }
    if !opts.tags.is_empty() {
        let tags: Vec<String> = opts.tags.iter().map(|t| q(t)).collect();
        text.push_str(&format!("tags = [{}]\n", tags.join(", ")));
    }
    if let Some(l) = opts.lifecycle {
        text.push_str(&format!("lifecycle = \"{l}\"\n"));
    }
    if !opts.inputs.is_empty() {
        let inputs: Vec<String> = opts.inputs.iter().map(|t| q(t)).collect();
        text.push_str(&format!("inputs = [{}]\n", inputs.join(", ")));
    }
    let command = |text: &mut String, table: &str| {
        text.push_str(&format!("\n[{table}]\n"));
        if opts.run.is_empty() {
            text.push_str(
                "run = [\"program\", \"--write-to\", \"{output}\"]   # TODO: the command\n",
            );
        } else {
            let run: Vec<String> = opts.run.iter().map(|a| q(a)).collect();
            text.push_str(&format!("run = [{}]\n", run.join(", ")));
        }
        if opts.stdout {
            text.push_str("stdout = true\n");
        }
    };
    match opts.kind {
        Kind::Source => {
            command(&mut text, "source");
            if let Some(i) = &opts.interval {
                text.push_str(&format!("interval = {}\n", q(i)));
            }
        }
        Kind::Derived => command(&mut text, "transform"),
        Kind::Authored => {
            if !opts.run.is_empty() {
                command(&mut text, "generator");
            }
            text.push_str(
                "\n# Map ranges of the document to the evidence they are based on:\n\
                 # [[claims]]\n\
                 # id = \"summary\"\n\
                 # select = { heading = \"## Summary\" }   # or { lines = \"10-20\" }, { marker = \"name\" }\n\
                 # evidence = [\"other-evidence-id\"]\n\
                 #\n\
                 # [[claims]]\n\
                 # id = \"background\"\n\
                 # select = { heading = \"## Background\" }\n\
                 # origin = \"primary\"                      # not backed by evidence\n",
            );
        }
    }
    let parsed = document::parse(&text, &evid);
    if let Some(d) = parsed.diagnostics.iter().find(|d| d.is_error()) {
        return Err(Error::Invalid(format!(
            "the new evidence file would be invalid: {d}"
        )));
    }
    if let Some(parent) = evid.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(&evid, text).map_err(|e| Error::io(&evid, e))?;
    Ok(evid)
}

/// The `subject` value for an evidence file at `evid`: relative to its directory inside the
/// project, absolute outside it.
fn subject_entry(layout: &Layout, evid: &Path, subject: &Path) -> String {
    let dir = evid.parent().unwrap_or(Path::new("."));
    if !inside(layout.root(), subject) {
        return subject.display().to_string();
    }
    let dir: Vec<_> = dir.components().collect();
    let target: Vec<_> = subject.components().collect();
    let shared_prefix = dir.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); dir.len() - shared_prefix];
    parts.extend(
        target[shared_prefix..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Derives an id from a subject file name: lowercase, invalid characters replaced by `-`.
pub(crate) fn derive_id(subject: &Path) -> String {
    let name = subject
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.split('.').next().unwrap_or(&name);
    let id: String = stem
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches('-');
    if id.is_empty() {
        "evidence".into()
    } else {
        id.to_owned()
    }
}
