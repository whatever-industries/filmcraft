//! Search a pinned revision for sequences that depend on a project item.
use super::{RECORD_LIMIT, bins, invalid, record_index, recovery};
use crate::MAX_PROJECT_BYTES;
use filmcraft_project::ItemKind;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::{io, path::Path};

#[derive(Serialize)]
pub struct Usage {
    pub revision: String,
    pub item: u64,
    pub sequences: Vec<SequenceUse>,
    pub filings: Vec<Filing>,
    pub complete: bool,
    pub bin_errors: BTreeMap<u64, String>,
    pub item_errors: BTreeMap<u64, String>,
}

#[derive(Serialize)]
pub struct SequenceUse {
    pub id: u64,
    pub name: String,
    pub direct: bool,
}

#[derive(Serialize)]
pub struct Filing {
    pub bin: bins::BinLocation,
    pub items: BTreeSet<u64>,
}

pub fn find_uses(folder: &Path, revision: &str, item: u64) -> io::Result<Usage> {
    let manifest = recovery::at(folder, revision)?;
    let mut budget = MAX_PROJECT_BYTES;
    let (envelope, root) = recovery::metadata(folder, &manifest, &mut budget)?;
    let inventory = recovery::inventory(envelope.pointer("/project/items"))?;
    if inventory.len() > RECORD_LIMIT {
        return Err(invalid("too many project items"));
    }
    if !inventory.contains(&item) {
        return Err(invalid(format!("item {item} absent from revision {revision}")));
    }
    drop(envelope);
    let index = record_index(&manifest.items);
    let mut item_errors = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut users = BTreeMap::<u64, BTreeSet<u64>>::new();
    for &id in inventory.iter().chain(index.keys().filter(|id| !inventory.contains(id))) {
        match recovery::read_item(folder, &index, &inventory, id, &mut budget) {
            Ok((record, _)) => {
                record.visit_references(|target| {
                    if inventory.contains(&target.0) {
                        users.entry(target.0).or_default().insert(id);
                    } else {
                        item_errors.entry(target.0).or_insert_with(|| format!("item {id} references absent item {}", target.0));
                    }
                });
                if matches!(record.kind, ItemKind::Sequence(_)) {
                    names.insert(id, record.name);
                }
            }
            Err(e) => {
                item_errors.insert(id, e.to_string());
            }
        }
    }

    let sequences = matching_sequences(item, &users, names);
    let wanted: BTreeSet<_> = std::iter::once(item).chain(sequences.iter().map(|s| s.id)).collect();
    let mut members = BTreeMap::<u64, BTreeSet<u64>>::new();
    let mut listing = bins::scan_bins(folder, &manifest, root, &mut budget, |bin, raw| {
        let mut file = |id: u64| {
            if !inventory.contains(&id) {
                item_errors.entry(id).or_insert_with(|| format!("bin {} lists absent item {id}", bin.id));
            }
            if wanted.contains(&id) {
                members.entry(bin.id).or_default().insert(id);
            }
        };
        for child in &bin.children {
            if let bins::Entry::Item(id) = child {
                file(*id);
            }
        }
        if let Some(extra) = raw.get("filed_sources") {
            for id in Vec::<u64>::deserialize(extra)? {
                file(id);
            }
        }
        Ok(())
    });
    let mut filings = Vec::new();
    for bin in listing.bins {
        if bin.path.is_none() {
            listing.errors.entry(bin.id).or_insert_with(|| format!("cannot verify path for bin {}", bin.id));
        }
        if let Some(items) = members.remove(&bin.id) {
            filings.push(Filing { bin, items });
        }
    }
    Ok(Usage {
        revision: revision.to_owned(),
        item,
        sequences,
        filings,
        complete: !listing.incomplete && item_errors.is_empty(),
        bin_errors: listing.errors,
        item_errors,
    })
}

fn matching_sequences(item: u64, users: &BTreeMap<u64, BTreeSet<u64>>, names: BTreeMap<u64, String>) -> Vec<SequenceUse> {
    // Walk verified edges backwards. Shared inputs and reference cycles each yield one row per sequence.
    let mut reached = BTreeSet::from([item]);
    let mut pending = vec![item];
    while let Some(target) = pending.pop() {
        for &parent in users.get(&target).into_iter().flatten() {
            if reached.insert(parent) {
                pending.push(parent);
            }
        }
    }
    let direct = users.get(&item);
    names
        .into_iter()
        .filter(|(id, _)| *id != item && reached.contains(id))
        .map(|(id, name)| SequenceUse { id, name, direct: direct.is_some_and(|set| set.contains(&id)) })
        .collect()
}
