//! Object store maintenance: `evidence store prune`.

use std::collections::HashSet;

use cas_fs::ObjectId;

use crate::error::Result;
use crate::history;
use crate::layout::Layout;
use crate::workspace::Workspace;

/// Snapshot ids referenced by evidence files (current snapshots, inputs, pins).
pub(crate) fn referenced_snapshots(ws: &Workspace) -> HashSet<ObjectId> {
    let mut out = HashSet::new();
    for pair in ws.pairs() {
        let Some(state) = &pair.file.state else {
            continue;
        };
        out.extend(state.snapshot);
        out.extend(state.inputs.values().copied());
        for claim in state.claims.values() {
            out.extend(claim.evidence.iter().map(|p| p.snapshot));
        }
    }
    out
}

/// Removes unreferenced objects. History references are kept unless `drop_history`.
pub(crate) fn prune(layout: &Layout, drop_history: bool) -> Result<cas_fs::PruneReport> {
    let _lock = layout.lock()?;
    let ws = Workspace::load(layout)?;
    let mut roots = referenced_snapshots(&ws);
    if !drop_history {
        roots.extend(
            history::referenced_snapshots(layout)?
                .iter()
                .filter_map(|s| s.parse::<ObjectId>().ok()),
        );
    }
    Ok(layout.store()?.prune(roots)?)
}
