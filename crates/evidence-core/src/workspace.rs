//! Discovery of evidence pairs, their relations, and the run graph of productions.
//!
//! Three relations connect pairs and are kept apart (INV 09): production inputs, claim
//! references, and the structural `members` relation. Only inputs and `members` order runs;
//! the run graph has one node per production.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::format::document::{self, EVID_SUFFIX};
use crate::format::{
    Diagnostic, Entry, EvidenceFile, Format, MemberState, RefVersion, compose_file,
};
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
    /// The evidence file is format version 1: readable, but it must be migrated before the
    /// tool records anything in it.
    pub legacy: bool,
}

/// How one pair relates to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// An input of a production: a change is a reason to re-run it.
    Input,
    /// A reference of a claim: used to evaluate the claim, never to run anything.
    Claim,
    /// The parent whose list this member belongs to.
    Member,
}

impl Relation {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Claim => "claim",
            Self::Member => "member",
        }
    }
}

/// A relation from a pair to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    /// The other pair's id.
    pub id: String,
    /// The relation.
    pub relation: Relation,
    /// Only through fixed revisions: the current version of `id` does not matter.
    pub fixed: bool,
}

/// A node of the run graph: one production of one pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Node {
    pub(crate) pair: String,
    pub(crate) production: String,
}

/// A member pair's place in its parent's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

    /// Relations to other pairs: production inputs, claim references (hand-written and
    /// generated), and the parent of a member. One edge per (id, relation); an edge is
    /// `fixed` when every reference behind it names a fixed revision. References to the pair
    /// itself are not edges.
    pub fn edges(&self) -> Vec<Edge> {
        let mut out: Vec<Edge> = Vec::new();
        let mut add = |id: &str, relation: Relation, fixed: bool| {
            if id == self.id {
                return;
            }
            match out
                .iter_mut()
                .find(|e| e.id == id && e.relation == relation)
            {
                Some(e) => e.fixed &= fixed,
                None => out.push(Edge {
                    id: id.to_owned(),
                    relation,
                    fixed,
                }),
            }
        };
        for input in self.file.productions.iter().flat_map(|p| &p.inputs) {
            add(input.id(), Relation::Input, input.is_fixed());
        }
        for (claim, _) in self.file.all_claims() {
            for r in &claim.evidence {
                add(r.id(), Relation::Claim, r.is_fixed());
            }
        }
        if let Some(m) = &self.membership {
            add(&m.parent, Relation::Member, false);
        }
        out
    }

    /// Ids of every pair this one relates to (any relation), deduplicated.
    pub(crate) fn dependencies(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in self.edges() {
            if !out.contains(&e.id) {
                out.push(e.id);
            }
        }
        out
    }

    /// Ids whose *current* version matters to this pair (edges that are not only fixed).
    pub(crate) fn current_dependencies(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in self.edges().into_iter().filter(|e| !e.fixed) {
            if !out.contains(&e.id) {
                out.push(e.id);
            }
        }
        out
    }

    /// Format of the subject: declared in the evidence file, or inferred from its extension.
    pub fn format(&self) -> Format {
        self.file
            .format
            .unwrap_or_else(|| Format::from_path(&self.subject_path))
    }
}

/// All pairs of a project.
#[derive(Debug, Clone)]
pub(crate) struct Workspace {
    store: cas_fs::Store,
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
        Self::from_paths(layout, &paths)
    }

    /// The object store.
    pub(crate) fn store(&self) -> &cas_fs::Store {
        &self.store
    }

    fn from_paths(layout: &Layout, paths: &[PathBuf]) -> Result<Self> {
        let mut ws = Self {
            store: layout.store()?,
            pairs: Vec::new(),
            by_id: HashMap::new(),
            member_files: Vec::new(),
            diagnostics: Vec::new(),
        };
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
            let legacy = parsed.legacy.is_some();
            ws.insert(layout, &mut by_subject, path, file, subject, None, legacy);
        }

        // Members: compose each definition from its parent's template and list entry.
        let mut lists: HashMap<String, Option<Vec<Entry>>> = HashMap::new();
        for pair in &ws.pairs {
            let Some(listing) = crate::membership::listing(&ws, pair) else {
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
            let parent = ws.get(&member.member_of);
            let parent_legacy = parent.is_some_and(|p| p.legacy);
            // A version 1 parent's template was converted in memory; its version 1 members
            // are composed from the template as written.
            let members = if parent_legacy {
                parent.and_then(|p| {
                    document::read(&p.evid_path)
                        .legacy
                        .and_then(|l| l.members.clone())
                })
            } else {
                parent.and_then(|p| p.file.members.clone())
            };
            let Some(members) = members else {
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
            let recorded = member
                .state
                .as_ref()
                .and_then(|s| s.get("member"))
                .and_then(|m| serde_json::from_value::<MemberState>(m.clone()).ok());
            let known = current.clone().or(recorded);
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
            let version = if parent_legacy { 1 } else { 2 };
            let (file, diagnostics) = compose_file(&members, member, &known.fields, version);
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
                parent_legacy || member.version == 1,
            );
        }

        ws.check_references();
        if let Err(Error::Cycle(cycle)) = ws.run_levels() {
            ws.diagnostics.push(Diagnostic::error(
                "cycle",
                format!("production cycle: {}", cycle.join(" -> ")),
            ));
        }
        Ok(ws)
    }

    /// Unknown references (a fixed revision may name evidence that no longer exists), and
    /// `satisfies` naming requirements the target does not declare.
    fn check_references(&mut self) {
        let mut found = Vec::new();
        for pair in &self.pairs {
            let refs = pair
                .file
                .productions
                .iter()
                .flat_map(|p| p.inputs.iter())
                .chain(
                    pair.file
                        .all_claims()
                        .into_iter()
                        .flat_map(|(c, _)| &c.evidence),
                );
            let mut reported: Vec<&str> = Vec::new();
            for r in refs {
                let target = self.get(r.id());
                if target.is_none()
                    && r.version() == RefVersion::Current
                    && !reported.contains(&r.id())
                {
                    reported.push(r.id());
                    found.push(
                        Diagnostic::error(
                            "unknown-ref",
                            format!("references unknown evidence `{}`", r.id()),
                        )
                        .in_file(&pair.evid_rel),
                    );
                }
                if let (Some(req), Some(target)) = (r.satisfies(), target)
                    && !target.file.requirements.iter().any(|q| q.id == req)
                {
                    found.push(
                        Diagnostic::error(
                            "unknown-requirement",
                            format!("`{}` declares no requirement `{req}`", target.id),
                        )
                        .in_file(&pair.evid_rel),
                    );
                }
            }
        }
        self.diagnostics.extend(found);
    }

    /// Adds a pair, unless its id or subject is taken.
    #[allow(clippy::too_many_arguments)]
    fn insert(
        &mut self,
        layout: &Layout,
        by_subject: &mut HashMap<PathBuf, String>,
        path: &Path,
        file: EvidenceFile,
        subject: PathBuf,
        membership: Option<Membership>,
        legacy: bool,
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
                legacy,
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

    /// Pairs this pair relates to (any relation) that exist in the workspace.
    pub(crate) fn dependencies(&self, id: &str) -> Vec<&Pair> {
        self.get(id)
            .map(|p| {
                p.dependencies()
                    .iter()
                    .filter_map(|d| self.get(d))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pairs that relate directly to `id` (any relation).
    pub(crate) fn dependents(&self, id: &str) -> Vec<&Pair> {
        self.pairs
            .iter()
            .filter(|p| p.dependencies().iter().any(|d| d == id))
            .collect()
    }

    /// Pairs whose current version of `id` matters (any relation that is not only through
    /// fixed revisions).
    pub(crate) fn current_dependents(&self, id: &str) -> Vec<&Pair> {
        self.pairs
            .iter()
            .filter(|p| p.current_dependencies().iter().any(|d| d == id))
            .collect()
    }

    /// Every pair whose current version of `id` matters, transitively, in discovery order.
    pub(crate) fn downstream(&self, id: &str) -> Vec<&Pair> {
        let mut seen = HashSet::new();
        let mut stack = vec![id.to_owned()];
        while let Some(current) = stack.pop() {
            for dep in self.current_dependents(&current) {
                if seen.insert(dep.id.clone()) {
                    stack.push(dep.id.clone());
                }
            }
        }
        self.pairs.iter().filter(|p| seen.contains(&p.id)).collect()
    }

    /// Every production, as a node of the run graph.
    pub(crate) fn nodes(&self) -> Vec<Node> {
        self.pairs
            .iter()
            .flat_map(|p| {
                p.file.productions.iter().map(|q| Node {
                    pair: p.id.clone(),
                    production: q.name.clone(),
                })
            })
            .collect()
    }

    /// The nodes a node must run after: the subject writer of each current input, the
    /// subject writer of a member's parent, and, for a production that does not write the
    /// subject, the subject writer of its own pair.
    pub(crate) fn node_dependencies(&self, node: &Node) -> Vec<Node> {
        let Some(pair) = self.get(&node.pair) else {
            return Vec::new();
        };
        let Some(production) = pair.file.production(&node.production) else {
            return Vec::new();
        };
        let writer = |id: &str| {
            self.get(id).and_then(|p| {
                p.file.subject_writer().map(|w| Node {
                    pair: p.id.clone(),
                    production: w.name.clone(),
                })
            })
        };
        let mut out: Vec<Node> = Vec::new();
        let mut add = |n: Option<Node>| {
            if let Some(n) = n
                && n != *node
                && !out.contains(&n)
            {
                out.push(n);
            }
        };
        for input in &production.inputs {
            if !input.is_fixed() {
                add(writer(input.id()));
            }
        }
        if let Some(m) = &pair.membership {
            add(writer(&m.parent));
        }
        if !production.writes_subject() {
            add(writer(&pair.id));
        }
        out
    }

    /// Productions grouped by depth in the run graph: level 0 depends on nothing, level n
    /// only on lower levels. Nodes within a level are independent. Fails on cycles.
    pub(crate) fn run_levels(&self) -> Result<Vec<Vec<Node>>> {
        let nodes = self.nodes();
        let deps: BTreeMap<Node, Vec<Node>> = nodes
            .iter()
            .map(|n| (n.clone(), self.node_dependencies(n)))
            .collect();
        let mut indegree: BTreeMap<&Node, usize> = deps.iter().map(|(n, d)| (n, d.len())).collect();
        let mut levels: Vec<Vec<Node>> = Vec::new();
        let mut done = 0;
        let mut current: Vec<&Node> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| *n)
            .collect();
        while !current.is_empty() {
            done += current.len();
            let mut next: Vec<&Node> = Vec::new();
            for n in &current {
                for (m, d) in &deps {
                    if d.contains(n)
                        && let Some(k) = indegree.get_mut(m)
                    {
                        *k -= 1;
                        if *k == 0 {
                            next.push(m);
                        }
                    }
                }
            }
            levels.push(current.iter().map(|n| (*n).clone()).collect());
            next.sort_unstable();
            next.dedup();
            current = next;
        }
        if done < nodes.len() {
            return Err(Error::Cycle(find_cycle(&deps)));
        }
        Ok(levels)
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

fn find_cycle(deps: &BTreeMap<Node, Vec<Node>>) -> Vec<String> {
    fn visit<'a>(
        deps: &'a BTreeMap<Node, Vec<Node>>,
        node: &'a Node,
        stack: &mut Vec<&'a Node>,
        done: &mut HashSet<&'a Node>,
    ) -> Option<Vec<String>> {
        let label = |n: &Node| format!("{}/{}", n.pair, n.production);
        if let Some(pos) = stack.iter().position(|s| *s == node) {
            let mut cycle: Vec<String> = stack[pos..].iter().map(|n| label(n)).collect();
            cycle.push(label(node));
            return Some(cycle);
        }
        if !done.insert(node) {
            return None;
        }
        stack.push(node);
        for dep in deps.get(node).into_iter().flatten() {
            if let Some(c) = visit(deps, dep, stack, done) {
                return Some(c);
            }
        }
        stack.pop();
        None
    }
    let mut done = HashSet::new();
    for n in deps.keys() {
        if let Some(c) = visit(deps, n, &mut Vec::new(), &mut done) {
            return c;
        }
    }
    Vec::new()
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
