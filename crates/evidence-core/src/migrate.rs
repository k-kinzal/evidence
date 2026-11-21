//! `evidence migrate`: rewriting format version 1 evidence files as version 2
//! (`docs/migration.md`).
//!
//! Nothing else migrates: reading commands convert version 1 files in memory only, and
//! commands that record something refuse them. A migration keeps a backup of every file it
//! rewrites, never invents what version 1 did not record (fingerprints, run ids, fixed
//! revisions), and is idempotent: a second run changes nothing.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::format::{Diagnostic, EvidenceFile, MemberFile, Severity, document};
use crate::history::{self, Event};
use crate::journal;
use crate::layout::Layout;
use crate::workspace::{MemberOnDisk, Pair, Workspace};

/// Options of [`migrate`].
#[derive(Debug, Clone, Default)]
pub struct MigrateOptions {
    /// Ids or paths (directories select everything under them). Empty: the whole project.
    pub targets: Vec<String>,
    /// Report what would change without writing anything.
    pub dry_run: bool,
}

/// What happened to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrateAction {
    /// Rewritten as format version 2 (or would be, in a dry run).
    Migrated,
    /// Already format version 2.
    Current,
    /// Not migrated: something cannot be converted without a decision (see `issues`).
    Blocked,
}

/// A problem or note found while converting.
#[derive(Debug, Clone, Serialize)]
pub struct MigrateIssue {
    /// `error` (blocks the file) or `warning`.
    pub severity: Severity,
    /// Stable code (`revision-unknown`, `generator-claims`, `generator-pins`, …).
    pub code: String,
    /// Explanation.
    pub message: String,
}

/// One file of a migration.
#[derive(Debug, Clone, Serialize)]
pub struct MigratedFile {
    /// Evidence file, relative to the project root.
    pub evid: String,
    /// Evidence id (`None` for a member file whose parent is not loaded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What happened.
    pub action: MigrateAction,
    /// Where the version 1 file was saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// Problems and notes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<MigrateIssue>,
}

/// Result of [`migrate`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct MigrateReport {
    /// Nothing was written.
    pub dry_run: bool,
    /// Files, in discovery order.
    pub files: Vec<MigratedFile>,
}

impl MigrateReport {
    /// Whether any selected file could not be migrated.
    pub fn blocked(&self) -> bool {
        self.files
            .iter()
            .any(|f| f.action == MigrateAction::Blocked)
    }
}

fn issues(diagnostics: &[Diagnostic]) -> Vec<MigrateIssue> {
    diagnostics
        .iter()
        .filter(|d| d.code != "legacy-format")
        .map(|d| MigrateIssue {
            severity: d.severity,
            code: d.code.to_string(),
            message: d.message.clone(),
        })
        .collect()
}

/// A rewrite to perform.
struct Rewrite {
    path: PathBuf,
    id: Option<String>,
    text: String,
    issues: Vec<MigrateIssue>,
}

/// Migrates the selected evidence files. A member file always migrates together with its
/// parent (its definition is composed from the parent's template).
pub(crate) fn migrate(
    layout: &Layout,
    cwd: &Path,
    now: Timestamp,
    opts: &MigrateOptions,
) -> Result<MigrateReport> {
    let _lock = if opts.dry_run {
        None
    } else {
        let lock = layout.lock()?;
        journal::recover(layout)?;
        Some(lock)
    };
    let ws = Workspace::load(layout)?;
    let selected: Vec<&Pair> = if opts.targets.is_empty() {
        ws.pairs().iter().collect()
    } else {
        ws.select(layout, cwd, &opts.targets)?
    };
    // Members bring their parent; parents bring their members.
    let mut parents: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for pair in &selected {
        let parent = pair.membership.as_ref().map_or(&pair.id, |m| &m.parent);
        if ws.get(parent).is_some_and(|p| p.file.members.is_some()) && !parents.contains(parent) {
            parents.push(parent.clone());
        }
        if !ids.contains(&pair.id) {
            ids.push(pair.id.clone());
        }
    }
    for parent in &parents {
        if !ids.contains(parent) {
            ids.push(parent.clone());
        }
        for p in ws.pairs() {
            if p.membership.as_ref().is_some_and(|m| &m.parent == parent) && !ids.contains(&p.id) {
                ids.push(p.id.clone());
            }
        }
    }

    let mut report = MigrateReport {
        dry_run: opts.dry_run,
        files: Vec::new(),
    };
    let mut rewrites: Vec<Rewrite> = Vec::new();
    let mut blocked_parents: Vec<String> = Vec::new();
    for pair in ws.pairs().iter().filter(|p| ids.contains(&p.id)) {
        if pair.membership.is_some() {
            continue;
        }
        match plan_pair(pair) {
            Planned::Current => report.files.push(MigratedFile {
                evid: pair.evid_rel.clone(),
                id: Some(pair.id.clone()),
                action: MigrateAction::Current,
                backup: None,
                issues: Vec::new(),
            }),
            Planned::Blocked(issues) => {
                if pair.file.members.is_some() {
                    blocked_parents.push(pair.id.clone());
                }
                report.files.push(MigratedFile {
                    evid: pair.evid_rel.clone(),
                    id: Some(pair.id.clone()),
                    action: MigrateAction::Blocked,
                    backup: None,
                    issues,
                });
            }
            Planned::Rewrite(r) => rewrites.push(r),
        }
    }
    for pair in ws.pairs().iter().filter(|p| ids.contains(&p.id)) {
        let Some(m) = &pair.membership else { continue };
        let Some(on_disk) = ws
            .member_files()
            .iter()
            .find(|f| f.evid_path == pair.evid_path)
        else {
            continue;
        };
        let parent = ws.get(&m.parent);
        let planned = if blocked_parents.contains(&m.parent) {
            Planned::Blocked(vec![MigrateIssue {
                severity: Severity::Error,
                code: "parent-blocked".into(),
                message: format!("its parent `{}` cannot be migrated", m.parent),
            }])
        } else {
            plan_member(pair, on_disk, parent)
        };
        match planned {
            Planned::Current => report.files.push(MigratedFile {
                evid: pair.evid_rel.clone(),
                id: Some(pair.id.clone()),
                action: MigrateAction::Current,
                backup: None,
                issues: Vec::new(),
            }),
            Planned::Blocked(issues) => report.files.push(MigratedFile {
                evid: pair.evid_rel.clone(),
                id: Some(pair.id.clone()),
                action: MigrateAction::Blocked,
                backup: None,
                issues,
            }),
            Planned::Rewrite(r) => rewrites.push(r),
        }
    }
    // A parent is written only when all of its members can be.
    let blocked_members: Vec<String> = report
        .files
        .iter()
        .filter(|f| f.action == MigrateAction::Blocked)
        .filter_map(|f| f.id.as_ref())
        .filter_map(|id| ws.get(id))
        .filter_map(|p| p.membership.as_ref().map(|m| m.parent.clone()))
        .collect();

    for r in rewrites {
        let parent_blocked = r.id.as_ref().is_some_and(|id| {
            blocked_members.contains(id)
                || ws
                    .get(id)
                    .and_then(|p| p.membership.as_ref())
                    .is_some_and(|m| blocked_members.contains(&m.parent))
        });
        let evid = layout.relative(&r.path);
        if parent_blocked {
            report.files.push(MigratedFile {
                evid,
                id: r.id,
                action: MigrateAction::Blocked,
                backup: None,
                issues: vec![MigrateIssue {
                    severity: Severity::Error,
                    code: "member-blocked".into(),
                    message: "a member of the same list cannot be migrated; the list migrates \
                              together"
                        .into(),
                }],
            });
            continue;
        }
        let backup = layout.meta_dir().join("backup").join("v1").join(&evid);
        if !opts.dry_run {
            let original = std::fs::read(&r.path).map_err(|e| Error::io(&r.path, e))?;
            if let Some(dir) = backup.parent() {
                std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
            }
            if !backup.exists() {
                std::fs::write(&backup, &original).map_err(|e| Error::io(&backup, e))?;
            }
            document::atomic_write(&r.path, r.text.as_bytes())?;
            if let Some(id) = &r.id {
                let mut event = Event::new(now, "migrate", "migrated");
                event.message = Some(format!(
                    "format version 1 -> 2 (backup: {})",
                    layout.relative(&backup)
                ));
                history::append(layout, id, &event)?;
            }
        }
        report.files.push(MigratedFile {
            evid,
            id: r.id,
            action: MigrateAction::Migrated,
            backup: Some(layout.relative(&backup)),
            issues: r.issues,
        });
    }
    report.files.sort_by(|a, b| a.evid.cmp(&b.evid));
    Ok(report)
}

enum Planned {
    Current,
    Blocked(Vec<MigrateIssue>),
    Rewrite(Rewrite),
}

/// Plans the rewrite of a stand-alone evidence file.
fn plan_pair(pair: &Pair) -> Planned {
    let Ok(text) = std::fs::read_to_string(&pair.evid_path) else {
        return Planned::Blocked(vec![MigrateIssue {
            severity: Severity::Error,
            code: "unreadable".into(),
            message: "cannot read the evidence file".into(),
        }]);
    };
    let parsed = document::parse(&text, &pair.evid_path);
    if parsed.legacy.is_none() {
        return Planned::Current;
    }
    let found = issues(&parsed.diagnostics);
    if found.iter().any(|i| i.severity == Severity::Error) {
        return Planned::Blocked(found);
    }
    let Some(file) = parsed.file else {
        return Planned::Blocked(found);
    };
    match render(&file) {
        Ok(text) => Planned::Rewrite(Rewrite {
            path: pair.evid_path.clone(),
            id: Some(pair.id.clone()),
            text,
            issues: found,
        }),
        Err(e) => Planned::Blocked(vec![MigrateIssue {
            severity: Severity::Error,
            code: "render".into(),
            message: e,
        }]),
    }
}

/// Plans the rewrite of a member file: its overrides converted against the parent's
/// template, its state converted with the composed version 1 definition.
fn plan_member(pair: &Pair, on_disk: &MemberOnDisk, parent: Option<&Pair>) -> Planned {
    let blocked = |code: &str, message: String| {
        Planned::Blocked(vec![MigrateIssue {
            severity: Severity::Error,
            code: code.into(),
            message,
        }])
    };
    let Ok(text) = std::fs::read_to_string(&on_disk.evid_path) else {
        return blocked("unreadable", "cannot read the member file".into());
    };
    let Ok(doc) = toml::from_str::<toml::Table>(&text) else {
        return blocked("unreadable", "the member file is not valid TOML".into());
    };
    let Ok(json) = serde_json::to_value(&doc) else {
        return blocked("unreadable", "the member file cannot be read".into());
    };
    let Some(Ok(member)) = MemberFile::from_json(&json) else {
        return blocked("unreadable", "not a valid member file".into());
    };
    let Some(parent) = parent else {
        return blocked(
            "parent-missing",
            format!("its parent `{}` is not loaded", member.member_of),
        );
    };
    if member.version != 1 && !parent.legacy {
        return Planned::Current;
    }
    let found: Vec<MigrateIssue> = issues(&on_disk_diagnostics(pair));
    if found.iter().any(|i| i.severity == Severity::Error) {
        return Planned::Blocked(found);
    }
    let overrides = if member.overrides.is_empty() {
        Map::new()
    } else if !parent.legacy {
        return blocked(
            "member-overrides",
            "the parent was migrated already; convert the overrides of this member by hand".into(),
        );
    } else {
        match convert_overrides(parent, &member.overrides) {
            Ok(o) => o,
            Err(e) => return blocked("member-overrides", e),
        }
    };
    let mut out = toml::Table::new();
    out.insert("version".into(), toml::Value::Integer(2));
    out.insert(
        "member_of".into(),
        toml::Value::String(member.member_of.clone()),
    );
    out.insert("member".into(), toml::Value::String(member.member.clone()));
    for (k, v) in overrides {
        match toml::Value::try_from(v) {
            Ok(v) => {
                out.insert(k, v);
            }
            Err(e) => return blocked("render", e.to_string()),
        }
    }
    let header: String =
        text.lines()
            .take_while(|l| l.starts_with('#'))
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
    let body = match toml::to_string(&out) {
        Ok(b) => b,
        Err(e) => return blocked("render", e.to_string()),
    };
    let state = pair.file.state();
    match document::replace_state(&format!("{header}{body}"), &state) {
        Ok(text) => Planned::Rewrite(Rewrite {
            path: on_disk.evid_path.clone(),
            id: Some(pair.id.clone()),
            text,
            issues: found,
        }),
        Err(e) => blocked("render", e),
    }
}

fn on_disk_diagnostics(pair: &Pair) -> Vec<Diagnostic> {
    // The composed member was converted in memory when the project was loaded; conversion
    // problems that block are the ones a stand-alone file would have.
    let mut out = Vec::new();
    for (claim, _) in pair.file.all_claims() {
        for r in &claim.evidence {
            if r.detail().revision_unknown {
                out.push(Diagnostic::error(
                    "revision-unknown",
                    format!(
                        "claim `{}`: the fixed revision of `{}` was never recorded",
                        claim.id,
                        r.key()
                    ),
                ));
            }
        }
    }
    out
}

/// Converts a version 1 member's overrides: the version 2 fields of (template + overrides)
/// that differ from the version 2 template. A changed command table becomes a full
/// `productions` override.
fn convert_overrides(
    parent: &Pair,
    overrides: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(&parent.evid_path).map_err(|e| e.to_string())?;
    let parsed = document::parse(&text, &parent.evid_path);
    let template = parsed
        .legacy
        .as_ref()
        .and_then(|l| l.members.as_ref())
        .map(|m| m.template.clone())
        .ok_or("the parent's version 1 template cannot be read")?;
    let mut merged = template.clone();
    for (k, v) in overrides {
        match (merged.get_mut(k), v) {
            (Some(Value::Object(a)), Value::Object(b)) => {
                for (bk, bv) in b {
                    a.insert(bk.clone(), bv.clone());
                }
            }
            _ => {
                merged.insert(k.clone(), v.clone());
            }
        }
    }
    let inherited = crate::format::v1::template_to_v2(&template)?;
    let mine = crate::format::v1::template_to_v2(&merged)?;
    Ok(mine
        .into_iter()
        .filter(|(k, v)| inherited.get(k) != Some(v))
        .collect())
}

/// Renders a version 2 evidence file with its state.
fn render(file: &EvidenceFile) -> Result<String, String> {
    let text = document::render(file)?;
    document::replace_state(&text, &file.state())
}
