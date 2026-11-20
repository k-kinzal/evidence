//! Discovery of evidence pairs and the dependency graph.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::format::document::{self, EVID_SUFFIX};
use crate::format::{Diagnostic, Entry, EvidenceFile, Format, MemberState, compose_file};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::layout::{DIR, Layout};

/// A subject and its evidence file.
#[derive(Debug, Clone)]
pub struct Pair {
    /// Evidence id.
    pub id: String,
    /// Absolute path of the `.evid` file.
    pub evid_path: PathBuf,
    /// Absolute path of the subject.
    pub subject_path: PathBuf,
    /// Evidence file path relative to the project root.
    pub evid_rel: String,
    /// Subject path relative to the project root.
    pub subject_rel: String,
    /// Parsed evidence file.
    pub file: EvidenceFile,
    /// The subject lies outside the project root (allowed, with a warning). Such subjects
    /// are never deleted by `remove`.
    pub outside_project: bool,
    /// Set when the pair is a member of a list (`member_of`).
    pub membership: Option<Membership>,
}

/// A member pair's place in its parent's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Membership {
    /// Id of the parent, whose subject is the list.
    pub parent: String,
    /// Key of the member's entry.
    pub key: String,
    /// Whether the parent's list has the entry now (`None` when the list is not available).
    pub listed: Option<bool>,
    /// The entry as listed now.
    #[serde(skip)]
    pub(crate) current: Option<MemberState>,
}

/// A member evidence file found on disk, whether or not its definition could be composed.
#[derive(Debug, Clone)]
pub(crate) struct MemberOnDisk {
    pub(crate) evid_path: PathBuf,
    pub(crate) parent: String,
    pub(crate) key: String,
}

impl Pair {
    /// Directory of the evidence file (base for relative paths in options).
    pub(crate) fn evid_dir(&self) -> &Path {
        self.evid_path.parent().unwrap_or(Path::new("."))
    }

    /// Ids this pair depends on: inputs, claim references, and the parent of a member.
    pub(crate) fn dependencies(&self) -> Vec<&str> {
        let mut deps = self.file.dependencies();
        if let Some(m) = &self.membership
            && !deps.contains(&m.parent.as_str())
        {
            deps.push(&m.parent);
        }
        deps
    }

    /// Format of the subject: declared in the evidence file, or inferred from its extension.
    pub fn format(&self) -> Format {
        self.file
            .format
            .unwrap_or_else(|| Format::from_path(&self.subject_path))
    }
}

/// All pairs of a project.
#[derive(Debug, Clone, Default)]
pub(crate) struct Workspace {
    pairs: Vec<Pair>,
    by_id: HashMap<String, usize>,
    member_files: Vec<MemberOnDisk>,
    /// Problems found while loading (parse errors, duplicate ids, unknown references, cycles).
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Workspace {
    /// Discovers and parses every evidence file of the project.
    pub(crate) fn load(layout: &Layout) -> Result<Self> {
        let root = layout.root().to_path_buf();
        let mut builder = ignore::WalkBuilder::new(&root);
        builder
            .hidden(true)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .ignore(false)
            .parents(false)
            .add_custom_ignore_filename(".evidenceignore")
            .sort_by_file_name(std::cmp::Ord::cmp);
        let patterns = &layout.config().config.discovery.ignore;
        if !patterns.is_empty() {
            let mut overrides = ignore::overrides::OverrideBuilder::new(&root);
            for p in patterns {
                overrides
                    .add(&format!("!{p}"))
                    .map_err(|e| Error::Config(format!("discovery.ignore `{p}`: {e}")))?;
            }
            builder.overrides(
                overrides
                    .build()
                    .map_err(|e| Error::Config(format!("discovery.ignore: {e}")))?,
            );
        }
        // Do not descend into the metadata directory or into directory subjects (a synced
        // repository may contain `.evid` files of its own).
        builder.filter_entry(|entry| {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == DIR) {
                return false;
            }
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                return !document::evid_path(path).is_file();
            }
            true
        });

        let mut paths = Vec::new();
        for entry in builder.build() {
            let entry = entry.map_err(|e| Error::Invalid(format!("walking project: {e}")))?;
            let path = entry.path();
            if entry.file_type().is_some_and(|t| t.is_file())
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(EVID_SUFFIX) && n.len() > EVID_SUFFIX.len())
            {
                paths.push(path.to_path_buf());
            }
        }
        Ok(Self::from_paths(layout, &paths))
    }

    fn from_paths(layout: &Layout, paths: &[PathBuf]) -> Self {
        let mut ws = Self::default();
        // Parse everything first: a directory subject declared by another evidence file (for
        // example a synced repository) may contain evidence files of its own, which are
        // content, not part of this project.
        let parsed: Vec<_> = paths
            .iter()
            .map(|path| {
                let parsed = document::read(path);
                let subject = parsed.file.as_ref().and_then(|f| subject_of(path, f));
                (path, parsed, subject)
            })
            .collect();
        let directory_subjects: Vec<PathBuf> = parsed
            .iter()
            .filter_map(|(_, _, s)| s.clone())
            .filter(|s| s.is_dir())
            .collect();
        let parsed: Vec<_> = parsed
            .into_iter()
            .filter(|(path, _, _)| !directory_subjects.iter().any(|d| path.starts_with(d)))
            .collect();
        let mut by_subject: HashMap<PathBuf, String> = HashMap::new();
        for (path, parsed, subject) in &parsed {
            let rel = layout.relative(path);
            ws.diagnostics
                .extend(parsed.diagnostics.iter().cloned().map(|d| d.in_file(&rel)));
            let (Some(file), Some(subject)) = (parsed.file.clone(), subject.clone()) else {
                continue;
            };
            ws.insert(layout, &mut by_subject, path, file, subject, None);
        }

        // Members: compose each definition from its parent's template and list entry.
        let mut lists: HashMap<String, Option<Vec<Entry>>> = HashMap::new();
        for pair in &ws.pairs {
            let Some(listing) = crate::membership::listing(pair) else {
                continue;
            };
            let entries = match listing {
                Ok(crate::format::List { entries, problems }) => {
                    let skipped = crate::membership::targets(layout, pair, &entries)
                        .into_iter()
                        .filter_map(|(e, t)| {
                            t.err().map(|why| format!("entry `{}`: {why}", e.key))
                        });
                    for p in problems.into_iter().chain(skipped) {
                        ws.diagnostics.push(
                            Diagnostic::warning("member-list", p)
                                .in_file(&pair.evid_rel)
                                .at_pointer("/members/key"),
                        );
                    }
                    Some(entries)
                }
                Err(e) => {
                    ws.diagnostics.push(
                        Diagnostic::warning("member-list", e)
                            .in_file(&pair.evid_rel)
                            .at_pointer("/members"),
                    );
                    None
                }
            };
            lists.insert(pair.id.clone(), entries);
        }
        for (path, parsed, _) in &parsed {
            let Some(member) = &parsed.member else {
                continue;
            };
            let rel = layout.relative(path);
            ws.member_files.push(MemberOnDisk {
                evid_path: (*path).clone(),
                parent: member.member_of.clone(),
                key: member.member.clone(),
            });
            let err = |code, msg: String| Diagnostic::error(code, msg).in_file(&rel);
            let Some(members) = ws
                .get(&member.member_of)
                .and_then(|p| p.file.members.clone())
            else {
                ws.diagnostics.push(err(
                    "unknown-ref",
                    format!(
                        "`member_of`: no evidence `{}` with `[members]`",
                        member.member_of
                    ),
                ));
                continue;
            };
            let list = lists.get(&member.member_of).and_then(Option::as_ref);
            let current = list
                .and_then(|l| l.iter().find(|e| e.key == member.member))
                .map(|e| e.state.clone());
            let known = current
                .clone()
                .or_else(|| member.state.as_ref().and_then(|s| s.member.clone()));
            let Some(known) = known else {
                ws.diagnostics.push(err(
                    "member",
                    format!(
                        "`{}` does not list `{}`, and this member was never produced",
                        member.member_of, member.member
                    ),
                ));
                continue;
            };
            let (file, diagnostics) = compose_file(&members, member, &known.fields);
            ws.diagnostics
                .extend(diagnostics.into_iter().map(|d| d.in_file(&rel)));
            let Some(file) = file else { continue };
            let Some(subject) = subject_of(path, &file) else {
                continue;
            };
            let membership = Membership {
                parent: member.member_of.clone(),
                key: member.member.clone(),
                listed: list.map(|_| current.is_some()),
                current,
            };
            ws.insert(
                layout,
                &mut by_subject,
                path,
                file,
                subject,
                Some(membership),
            );
        }

        for pair in &ws.pairs {
            for dep in pair.file.dependencies() {
                if !ws.by_id.contains_key(dep) {
                    ws.diagnostics.push(
                        Diagnostic::error(
                            "unknown-ref",
                            format!("references unknown evidence `{dep}`"),
                        )
                        .in_file(&pair.evid_rel),
                    );
                }
            }
        }
        if let Err(Error::Cycle(cycle)) = ws.topological_order() {
            ws.diagnostics.push(Diagnostic::error(
                "cycle",
                format!("dependency cycle: {}", cycle.join(" -> ")),
            ));
        }
        ws
    }

    /// Adds a pair, unless its id or subject is taken.
    fn insert(
        &mut self,
        layout: &Layout,
        by_subject: &mut HashMap<PathBuf, String>,
        path: &Path,
        file: EvidenceFile,
        subject: PathBuf,
        membership: Option<Membership>,
    ) {
        let ws = self;
        let rel = layout.relative(path);
        {
            if let Some(&existing) = ws.by_id.get(&file.id) {
                ws.diagnostics.push(
                    Diagnostic::error(
                        "duplicate-id",
                        format!(
                            "id `{}` is already used by {}",
                            file.id, ws.pairs[existing].evid_rel
                        ),
                    )
                    .in_file(&rel),
                );
                return;
            }
            if let Some(other) = by_subject.get(&subject) {
                ws.diagnostics.push(
                    Diagnostic::error(
                        "duplicate-subject",
                        format!(
                            "{} is already described by {other}",
                            layout.relative(&subject)
                        ),
                    )
                    .in_file(&rel)
                    .at_pointer("/subject"),
                );
                return;
            }
            let outside_project = !inside(layout.root(), &subject);
            if outside_project {
                ws.diagnostics.push(
                    Diagnostic::warning(
                        "subject-outside-project",
                        format!(
                            "the subject {} is outside the project: a checkout of the project \
                             alone does not contain it, and `remove` leaves it in place",
                            subject.display()
                        ),
                    )
                    .in_file(&rel)
                    .at_pointer("/subject"),
                );
            }
            by_subject.insert(subject.clone(), rel.clone());
            ws.by_id.insert(file.id.clone(), ws.pairs.len());
            ws.pairs.push(Pair {
                id: file.id.clone(),
                subject_rel: layout.relative(&subject),
                evid_path: path.to_path_buf(),
                subject_path: subject,
                evid_rel: rel,
                file,
                outside_project,
                membership,
            });
        }
    }

    /// Every member evidence file on disk.
    pub(crate) fn member_files(&self) -> &[MemberOnDisk] {
        &self.member_files
    }

    /// All pairs, in discovery order (sorted by path).
    pub(crate) fn pairs(&self) -> &[Pair] {
        &self.pairs
    }

    /// Looks up a pair by id.
    pub(crate) fn get(&self, id: &str) -> Option<&Pair> {
        self.by_id.get(id).map(|&i| &self.pairs[i])
    }

    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut Pair> {
        self.by_id.get(id).copied().map(move |i| &mut self.pairs[i])
    }

    /// Ids this pair depends on that exist in the workspace.
    pub(crate) fn dependencies(&self, id: &str) -> Vec<&Pair> {
        self.get(id)
            .map(|p| {
                p.dependencies()
                    .into_iter()
                    .filter_map(|d| self.get(d))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pairs that depend directly on `id`.
    pub(crate) fn dependents(&self, id: &str) -> Vec<&Pair> {
        self.pairs
            .iter()
            .filter(|p| p.dependencies().contains(&id))
            .collect()
    }

    /// Every pair downstream of `id` (transitively), in topological order.
    pub(crate) fn downstream(&self, id: &str) -> Vec<&Pair> {
        let mut seen = HashSet::new();
        let mut stack = vec![id.to_owned()];
        while let Some(current) = stack.pop() {
            for dep in self.dependents(&current) {
                if seen.insert(dep.id.clone()) {
                    stack.push(dep.id.clone());
                }
            }
        }
        self.topological_order()
            .unwrap_or_else(|_| self.pairs.iter().collect())
            .into_iter()
            .filter(|p| seen.contains(&p.id))
            .collect()
    }

    /// Pairs ordered so that dependencies come first. Fails on cycles.
    pub(crate) fn topological_order(&self) -> Result<Vec<&Pair>> {
        Ok(self.levels()?.into_iter().flatten().collect())
    }

    /// Pairs grouped by depth: level 0 has no (known) dependencies, level n depends only on
    /// lower levels. Pairs within a level are independent.
    pub(crate) fn levels(&self) -> Result<Vec<Vec<&Pair>>> {
        let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
        for p in &self.pairs {
            let deps = p
                .dependencies()
                .into_iter()
                .filter(|d| self.by_id.contains_key(*d))
                .count();
            indegree.insert(&p.id, deps);
        }
        let mut levels = Vec::new();
        let mut done = 0;
        let mut current: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        while !current.is_empty() {
            done += current.len();
            let mut next = Vec::new();
            for id in &current {
                for dependent in self.dependents(id) {
                    if let Some(d) = indegree.get_mut(dependent.id.as_str()) {
                        *d -= 1;
                        if *d == 0 {
                            next.push(dependent.id.as_str());
                        }
                    }
                }
            }
            levels.push(current.iter().filter_map(|id| self.get(id)).collect());
            next.sort_unstable();
            next.dedup();
            current = next;
        }
        if done < self.pairs.len() {
            return Err(Error::Cycle(self.find_cycle()));
        }
        Ok(levels)
    }

    fn find_cycle(&self) -> Vec<String> {
        fn visit<'a>(
            ws: &'a Workspace,
            id: &'a str,
            stack: &mut Vec<&'a str>,
            done: &mut HashSet<&'a str>,
        ) -> Option<Vec<String>> {
            if let Some(pos) = stack.iter().position(|s| *s == id) {
                let mut cycle: Vec<String> = stack[pos..].iter().map(|s| (*s).to_owned()).collect();
                cycle.push(id.to_owned());
                return Some(cycle);
            }
            if !done.insert(id) {
                return None;
            }
            stack.push(id);
            for dep in ws.get(id).map(Pair::dependencies).unwrap_or_default() {
                if ws.get(dep).is_some()
                    && let Some(c) = visit(ws, dep, stack, done)
                {
                    return Some(c);
                }
            }
            stack.pop();
            None
        }
        let mut done = HashSet::new();
        for p in &self.pairs {
            if let Some(c) = visit(self, &p.id, &mut Vec::new(), &mut done) {
                return c;
            }
        }
        Vec::new()
    }

    /// Resolves targets (ids or paths, relative to `cwd`) to pairs.
    ///
    /// A path selects the pair whose subject or evidence file it names, or every pair under it
    /// when it is a directory.
    pub(crate) fn select(
        &self,
        layout: &Layout,
        cwd: &Path,
        targets: &[String],
    ) -> Result<Vec<&Pair>> {
        let mut out: Vec<&Pair> = Vec::new();
        for target in targets {
            let before = out.len();
            if let Some(p) = self.get(target) {
                out.push(p);
            } else {
                let path = absolutize(cwd, Path::new(target));
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                for p in &self.pairs {
                    let hit = p.subject_path == path
                        || p.evid_path == path
                        || p.subject_path.starts_with(&path)
                            && path.is_dir()
                            && path != p.subject_path
                        || p.evid_path.starts_with(&path) && path.is_dir();
                    if hit {
                        out.push(p);
                    }
                }
                if path == layout.root() {
                    out.extend(self.pairs.iter());
                }
            }
            if out.len() == before {
                return Err(Error::NotFound(target.clone()));
            }
        }
        let mut seen = HashSet::new();
        out.retain(|p| seen.insert(p.id.clone()));
        Ok(out)
    }
}

/// Where the subject of the evidence file at `evid_path` lives: its declared `subject`
/// (relative to the evidence file's directory), or the sidecar default.
pub(crate) fn subject_of(evid_path: &Path, file: &EvidenceFile) -> Option<PathBuf> {
    match &file.subject {
        None => document::subject_path(evid_path),
        Some(declared) => Some(normalize(&evid_path.parent()?.join(declared))),
    }
}

/// Whether `path` lies inside `root` (also through a symlinked parent directory).
pub(crate) fn inside(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        || path
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .is_some_and(|p| p.starts_with(root))
}

/// Resolves `.` and `..` without touching the file system (the subject may not exist yet).
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn absolutize(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}
