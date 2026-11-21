//! Dependency trees (`evidence tree`, `evidence tree --invert`).

use std::collections::HashSet;

use serde::Serialize;

use crate::format::ContentMode;
use crate::workspace::{Relation, Workspace};

/// A node of a dependency tree.
#[derive(Debug, Clone, Serialize)]
pub struct TreeNode {
    /// Evidence id.
    pub id: String,
    /// Whether the subject is editable or produced.
    pub content: ContentMode,
    /// How this node relates to its parent in the tree (`None` at the root). Several
    /// relations when the pairs are connected more than one way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
    /// Only fixed revisions connect this node to its parent.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fixed: bool,
    /// Already shown elsewhere in the tree (children omitted), like `cargo tree`'s `(*)`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub repeated: bool,
    /// Children: dependencies (or dependents when inverted).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeNode>,
}

/// Builds the tree rooted at `id`: dependencies, or dependents when `invert`.
pub(crate) fn tree(
    ws: &Workspace,
    id: &str,
    invert: bool,
    depth: Option<usize>,
) -> Option<TreeNode> {
    let mut seen = HashSet::new();
    build(ws, id, invert, depth, &mut seen)
}

/// Relations between `from` (the dependent) and `to`, and whether all are fixed.
fn relations(ws: &Workspace, from: &str, to: &str) -> (Vec<Relation>, bool) {
    let edges: Vec<_> = ws
        .get(from)
        .map(|p| p.edges().into_iter().filter(|e| e.id == to).collect())
        .unwrap_or_default();
    let mut rels: Vec<Relation> = edges.iter().map(|e| e.relation).collect();
    rels.sort_unstable();
    let fixed = !edges.is_empty() && edges.iter().all(|e| e.fixed);
    (rels, fixed)
}

fn build(
    ws: &Workspace,
    id: &str,
    invert: bool,
    depth: Option<usize>,
    seen: &mut HashSet<String>,
) -> Option<TreeNode> {
    let pair = ws.get(id)?;
    let mut node = TreeNode {
        id: pair.id.clone(),
        content: pair.file.content_mode(),
        relations: Vec::new(),
        fixed: false,
        repeated: false,
        children: Vec::new(),
    };
    if !seen.insert(pair.id.clone()) {
        node.repeated = true;
        return Some(node);
    }
    if depth == Some(0) {
        return Some(node);
    }
    let next = if invert {
        ws.dependents(id)
    } else {
        ws.dependencies(id)
    };
    for child in next {
        if let Some(mut c) = build(ws, &child.id, invert, depth.map(|d| d - 1), seen) {
            let (rels, fixed) = if invert {
                relations(ws, &child.id, id)
            } else {
                relations(ws, id, &child.id)
            };
            c.relations = rels;
            c.fixed = fixed;
            node.children.push(c);
        }
    }
    Some(node)
}

/// Roots of the forest: pairs nothing depends on (or, inverted, pairs with no dependencies).
pub(crate) fn roots(ws: &Workspace, invert: bool) -> Vec<String> {
    ws.pairs()
        .iter()
        .filter(|p| {
            if invert {
                ws.dependencies(&p.id).is_empty()
            } else {
                ws.dependents(&p.id).is_empty()
            }
        })
        .map(|p| p.id.clone())
        .collect()
}

/// Renders the whole graph in Graphviz DOT.
pub(crate) fn dot(ws: &Workspace) -> String {
    let mut out = String::from(
        "digraph evidence {\n  rankdir=LR;\n  node [shape=box, fontname=\"Helvetica\"];\n",
    );
    for p in ws.pairs() {
        let shape = match p.file.content_mode() {
            ContentMode::Produced => "box",
            ContentMode::Editable => "note",
        };
        out.push_str(&format!("  \"{}\" [shape={shape}];\n", p.id));
    }
    for p in ws.pairs() {
        for e in p.edges() {
            if ws.get(&e.id).is_none() {
                continue;
            }
            let style = match (e.relation, e.fixed) {
                (Relation::Input, false) => "solid",
                (Relation::Input, true) => "bold",
                (Relation::Claim, false) => "dashed",
                (Relation::Claim, true) => "dotted",
                (Relation::Member, _) => "tapered",
            };
            out.push_str(&format!(
                "  \"{}\" -> \"{}\" [style={style}, label=\"{}{}\"];\n",
                e.id,
                p.id,
                e.relation.as_str(),
                if e.fixed { "@rev" } else { "" }
            ));
        }
    }
    out.push_str("}\n");
    out
}
