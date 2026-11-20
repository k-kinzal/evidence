//! Dependency trees (`evidence tree`, `evidence tree --invert`).

use std::collections::HashSet;

use serde::Serialize;

use crate::workspace::Workspace;

/// A node of a dependency tree.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct TreeNode {
    /// Evidence id.
    pub id: String,
    /// Kind.
    pub kind: String,
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
        kind: pair.file.kind.to_string(),
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
        if let Some(c) = build(ws, &child.id, invert, depth.map(|d| d - 1), seen) {
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
        let shape = match p.file.kind {
            crate::format::Kind::Source => "cylinder",
            crate::format::Kind::Derived => "box",
            crate::format::Kind::Authored => "note",
        };
        out.push_str(&format!("  \"{}\" [shape={shape}];\n", p.id));
    }
    for p in ws.pairs() {
        for dep in ws.dependencies(&p.id) {
            out.push_str(&format!("  \"{}\" -> \"{}\";\n", dep.id, p.id));
        }
    }
    out.push_str("}\n");
    out
}
