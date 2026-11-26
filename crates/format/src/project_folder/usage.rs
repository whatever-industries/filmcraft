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
    UsageIndex::read_for(folder, revision, Some(item))?.lookup(item)
}

/// Verified relationships from one immutable save, including gaps observed at scan time.
#[derive(Debug, PartialEq)]
pub struct UsageIndex {
    revision: String,
    inventory: BTreeSet<u64>,
    users: BTreeMap<u64, BTreeSet<u64>>,
    names: BTreeMap<u64, String>,
    locations: BTreeMap<u64, Vec<u64>>,
    bins: BTreeMap<u64, bins::BinLocation>,
    complete: bool,
    bin_errors: BTreeMap<u64, String>,
    item_errors: BTreeMap<u64, String>,
}

impl UsageIndex {
    pub fn read(folder: &Path, revision: &str) -> io::Result<Self> {
        Self::read_for(folder, revision, None)
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    fn read_for(folder: &Path, revision: &str, requested: Option<u64>) -> io::Result<Self> {
        let manifest = recovery::at(folder, revision)?;
        let mut budget = MAX_PROJECT_BYTES;
        let (envelope, root) = recovery::metadata(folder, &manifest, &mut budget)?;
        let inventory = recovery::inventory(envelope.pointer("/project/items"))?;
        if inventory.len() > RECORD_LIMIT {
            return Err(invalid("too many project items"));
        }
        if let Some(item) = requested
            && !inventory.contains(&item)
        {
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

        let mut locations = BTreeMap::<u64, Vec<u64>>::new();
        let mut listing = bins::scan_bins(folder, &manifest, root, &mut budget, |bin, raw| {
            let mut file = |id: u64| {
                if !inventory.contains(&id) {
                    item_errors.entry(id).or_insert_with(|| format!("bin {} lists absent item {id}", bin.id));
                } else {
                    let homes = locations.entry(id).or_default();
                    if homes.last() != Some(&bin.id) {
                        homes.push(bin.id);
                    }
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
        let mut bins = BTreeMap::new();
        for bin in listing.bins {
            if bin.path.is_none() {
                listing.errors.entry(bin.id).or_insert_with(|| format!("cannot verify path for bin {}", bin.id));
            }
            bins.insert(bin.id, bin);
        }
        Ok(Self {
            revision: revision.to_owned(),
            inventory,
            users,
            names,
            locations,
            bins,
            complete: !listing.incomplete && item_errors.is_empty(),
            bin_errors: listing.errors,
            item_errors,
        })
    }

    pub fn lookup(&self, item: u64) -> io::Result<Usage> {
        if !self.inventory.contains(&item) {
            return Err(invalid(format!("item {item} absent from revision {}", self.revision)));
        }
        let sequences = self.matching_sequences(item);
        let mut members = BTreeMap::<u64, BTreeSet<u64>>::new();
        for id in std::iter::once(item).chain(sequences.iter().map(|seq| seq.id)) {
            for &bin in self.locations.get(&id).into_iter().flatten() {
                members.entry(bin).or_default().insert(id);
            }
        }
        let filings = members.into_iter().filter_map(|(id, items)| self.bins.get(&id).map(|bin| Filing { bin: bin.clone(), items })).collect();
        Ok(Usage {
            revision: self.revision.clone(),
            item,
            sequences,
            filings,
            complete: self.complete,
            bin_errors: self.bin_errors.clone(),
            item_errors: self.item_errors.clone(),
        })
    }

    fn matching_sequences(&self, item: u64) -> Vec<SequenceUse> {
        // Walk verified edges backwards. Shared inputs and reference cycles each yield one row per sequence.
        let mut reached = BTreeSet::from([item]);
        let mut pending = vec![item];
        while let Some(target) = pending.pop() {
            for &parent in self.users.get(&target).into_iter().flatten() {
                if reached.insert(parent) {
                    pending.push(parent);
                }
            }
        }
        let direct = self.users.get(&item);
        reached.remove(&item);
        reached
            .into_iter()
            .filter_map(|id| self.names.get(&id).map(|name| SequenceUse { id, name: name.clone(), direct: direct.is_some_and(|set| set.contains(&id)) }))
            .collect()
    }
}
