//! Object store maintenance: `evidence store prune`.

use std::collections::HashSet;

use cas_fs::ObjectId;

use crate::error::Result;
use crate::history;
use crate::layout::Layout;
use crate::workspace::Workspace;

/// Snapshot ids the current project needs (GC roots): adopted content, the inputs of run
/// records, the subject and reference versions of pins, and every fixed revision written in
/// a definition (even without any state).
pub(crate) fn referenced_snapshots(ws: &Workspace) -> HashSet<ObjectId> {
    let mut out = HashSet::new();
    for pair in ws.pairs() {
        for production in &pair.file.productions {
            out.extend(
                production
                    .inputs
                    .iter()
                    .filter_map(crate::format::Reference::revision),
            );
        }
        for (claim, _) in pair.file.all_claims() {
            out.extend(
                claim
                    .evidence
                    .iter()
                    .filter_map(crate::format::Reference::revision),
            );
        }
        let Some(state) = &pair.file.state else {
            continue;
        };
        out.extend(state.content.as_ref().map(|c| c.snapshot));
        for run in state.runs.values() {
            out.extend(run.subject);
            out.extend(run.inputs.values().map(|i| i.snapshot));
        }
        for pin in state.pins.values() {
            out.extend(pin.content.snapshot);
            out.extend(pin.evidence.iter().map(|p| p.snapshot));
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
