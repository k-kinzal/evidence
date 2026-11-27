//! Snapshot encryption follows the versions used to produce or substantiate a subject.

use std::collections::HashSet;

use crate::format::{ClaimSource, ContentMode, Reference, SnapshotStorage};
use crate::snapshot::{ResolveError, Resolver};
use crate::workspace::Pair;

#[derive(Default)]
pub(crate) struct Dependencies {
    any: bool,
    blocker: Option<String>,
}

impl Dependencies {
    pub(crate) fn add(&mut self, id: &str, storage: Option<SnapshotStorage>) {
        self.any = true;
        if storage != Some(SnapshotStorage::Plain) && self.blocker.is_none() {
            let reason = if storage.is_some() {
                "is encrypted"
            } else {
                "has unknown storage"
            };
            self.blocker = Some(format!("`{id}` {reason}"));
        }
    }

    pub(crate) fn decide(
        &self,
        resolver: &Resolver<'_>,
        pair: &Pair,
    ) -> Result<SnapshotStorage, ResolveError> {
        if !resolver.ws.encryption_enabled() {
            return Ok(SnapshotStorage::Plain);
        }
        match pair.file.storage.as_ref().and_then(|s| s.encryption) {
            Some(false) => match &self.blocker {
                None => Ok(SnapshotStorage::Plain),
                Some(reason) => Err(ResolveError::StorageConflict(format!(
                    "`{}` requests plaintext snapshots ([storage] encryption = false), but {reason}",
                    pair.id
                ))),
            },
            None if self.any && self.blocker.is_none() => Ok(SnapshotStorage::Plain),
            Some(true) | None => Ok(SnapshotStorage::Encrypted),
        }
    }
}

pub(crate) fn subject_dependencies(
    pair: &Pair,
    replacing_claims: Option<&str>,
    reference_storage: &mut impl FnMut(&Reference) -> Option<SnapshotStorage>,
) -> Dependencies {
    let mut deps = Dependencies::default();
    if let Some(membership) = &pair.membership {
        let storage = membership
            .current
            .as_ref()
            .and_then(|_| reference_storage(&Reference::Id(membership.parent.clone())));
        deps.add(&membership.parent, storage);
    }
    if pair
        .file
        .state
        .as_ref()
        .is_some_and(|s| s.protected.is_some())
        && pair
            .file
            .productions
            .iter()
            .any(|p| p.writes_claims() && Some(p.name.as_str()) != replacing_claims)
    {
        deps.add("previous evidence state", None);
    }
    for (claim, source) in pair.file.all_claims() {
        if let ClaimSource::Generated(production) = source {
            if Some(production) == replacing_claims {
                continue;
            }
            if let Some(state) = &pair.file.state {
                if let Some(run) = state.runs.get(production) {
                    for input in run.inputs.values() {
                        deps.add(&input.id, input.storage);
                    }
                }
                if let Some(pin) = state.pins.get(&claim.id) {
                    for (index, reference) in claim.evidence.iter().enumerate() {
                        if reference.id() != pair.id || reference.is_fixed() {
                            let storage = pin
                                .evidence
                                .get(index)
                                .filter(|p| p.key == reference.key())
                                .and_then(|p| p.storage);
                            deps.add(reference.id(), storage);
                        }
                    }
                    continue;
                }
            }
        }
        for reference in &claim.evidence {
            if reference.id() != pair.id || reference.is_fixed() {
                deps.add(reference.id(), reference_storage(reference));
            }
        }
    }
    deps
}

pub(crate) fn reference_storage(
    resolver: &Resolver<'_>,
    reference: &Reference,
) -> Option<SnapshotStorage> {
    reference_class(resolver, reference, &mut HashSet::new())
}

fn reference_class(
    resolver: &Resolver<'_>,
    reference: &Reference,
    visiting: &mut HashSet<String>,
) -> Option<SnapshotStorage> {
    let pair = resolver.ws.get(reference.id())?;
    if reference.is_fixed() {
        return resolver
            .fixed(Some(pair), reference.revision()?)
            .ok()?
            .storage();
    }
    current_storage(resolver, pair, visiting)
}

fn current_storage(
    resolver: &Resolver<'_>,
    pair: &Pair,
    visiting: &mut HashSet<String>,
) -> Option<SnapshotStorage> {
    match pair.file.content_mode() {
        ContentMode::Produced => resolver.current(pair).ok()?.storage(),
        ContentMode::Editable if pair.subject_path.exists() => {
            editable_class(resolver, pair, visiting).ok()
        }
        ContentMode::Editable => None,
    }
}

pub(crate) fn editable(
    resolver: &Resolver<'_>,
    pair: &Pair,
) -> Result<SnapshotStorage, ResolveError> {
    editable_class(resolver, pair, &mut HashSet::new())
}

fn editable_class(
    resolver: &Resolver<'_>,
    pair: &Pair,
    visiting: &mut HashSet<String>,
) -> Result<SnapshotStorage, ResolveError> {
    if !resolver.ws.encryption_enabled() {
        return Ok(SnapshotStorage::Plain);
    }
    if !visiting.insert(pair.id.clone()) {
        return Ok(SnapshotStorage::Encrypted);
    }
    let deps = subject_dependencies(pair, None, &mut |reference| {
        reference_class(resolver, reference, visiting)
    });
    visiting.remove(&pair.id);
    deps.decide(resolver, pair)
}
