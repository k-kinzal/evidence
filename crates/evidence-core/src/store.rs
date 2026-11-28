//! Object store maintenance: `evidence store prune`.

use std::collections::HashSet;

use cas_fs::{ObjectId, Storage};

use crate::error::Result;
use crate::history;
use crate::layout::Layout;
use crate::workspace::Workspace;

/// Snapshot ids the current project needs (GC roots): adopted content, the inputs of run
/// records, the subject and reference versions of pins, and every fixed revision written in
/// a definition (even without any state).
fn referenced_snapshots(ws: &Workspace) -> HashSet<(ObjectId, Option<Storage>)> {
    let mut out = HashSet::new();
    for pair in ws.pairs() {
        for production in &pair.file.productions {
            out.extend(
                production
                    .inputs
                    .iter()
                    .filter_map(|r| r.revision().map(|id| (id, None))),
            );
        }
        for (claim, _) in pair.file.all_claims() {
            out.extend(
                claim
                    .evidence
                    .iter()
                    .filter_map(|r| r.revision().map(|id| (id, None))),
            );
        }
        let Some(state) = &pair.file.state else {
            continue;
        };
        out.extend(
            state
                .content
                .as_ref()
                .map(|c| (c.snapshot, c.storage.map(Into::into))),
        );
        for run in state.runs.values() {
            out.extend(
                run.subject
                    .map(|id| (id, run.subject_storage.map(Into::into))),
            );
            out.extend(
                run.inputs
                    .values()
                    .map(|i| (i.snapshot, i.storage.map(Into::into))),
            );
        }
        for pin in state.pins.values() {
            out.extend(
                pin.content
                    .snapshot
                    .map(|id| (id, pin.content.storage.map(Into::into))),
            );
            out.extend(
                pin.evidence
                    .iter()
                    .map(|p| (p.snapshot, p.storage.map(Into::into))),
            );
        }
    }
    out
}

/// Removes unreferenced objects. History references are kept unless `drop_history`.
pub(crate) fn prune(layout: &Layout, drop_history: bool) -> Result<cas_fs::PruneReport> {
    let _lock = layout.lock()?;
    crate::journal::recover(layout)?;
    prune_locked(layout, drop_history, std::iter::empty())
}

pub(crate) fn prune_locked(
    layout: &Layout,
    drop_history: bool,
    persisted: impl IntoIterator<Item = ObjectId>,
) -> Result<cas_fs::PruneReport> {
    let ws = Workspace::load(layout)?;
    let mut roots = referenced_snapshots(&ws);
    roots.extend(persisted.into_iter().map(|id| (id, None)));
    for pair in ws.pairs() {
        roots.extend(crate::assessment::roots(pair)?);
    }
    if !drop_history {
        roots.extend(
            history::referenced_snapshots(layout)?
                .iter()
                .filter_map(|(s, class)| {
                    s.parse::<ObjectId>()
                        .ok()
                        .map(|id| (id, class.map(Into::into)))
                }),
        );
    }
    Ok(layout.store()?.prune_versions(roots)?)
}
