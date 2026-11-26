//! Read the selected hierarchy and its dependency closure, retaining verified siblings on error.
use super::Selection;
use crate::MAX_PROJECT_BYTES;
use crate::project_folder::{
    Manifest,
    bins::{self, Entry},
    invalid, record_index, recovery,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

#[derive(Default)]
pub(super) struct Snapshot {
    pub cut: Option<super::sequence::Cut>,
    pub records: BTreeMap<Entry, Value>,
    pub ambiguous: BTreeSet<Entry>,
    pub parents: BTreeMap<Entry, Option<u64>>,
    pub children: BTreeMap<u64, Vec<Entry>>,
    pub sidecars: BTreeMap<u64, Value>,
    pub errors: Vec<(Entry, String)>,
    pub hierarchy_complete: bool,
    pub catalog: BTreeSet<Entry>,
}

impl Snapshot {
    pub fn read(folder: &Path, m: &Manifest, selection: Selection) -> io::Result<Self> {
        let mut budget = MAX_PROJECT_BYTES;
        let (envelope, root) = recovery::metadata(folder, m, &mut budget)?;
        let inventory = recovery::inventory(envelope.pointer("/project/items"))?;
        let bins = record_index(&m.bins);
        let items = record_index(&m.items);
        let mut result = Self { hierarchy_complete: true, ..Self::default() };
        result.catalog.extend(bins.keys().copied().map(Entry::Bin));
        result.catalog.extend(items.keys().chain(&inventory).copied().map(Entry::Item));
        let selected = selection.key();
        if !result.catalog.contains(&selected) {
            return Ok(result);
        }
        let mut pending_items = Vec::new();
        let mut pending_bins = Vec::new();
        result.parents.insert(selected, None);
        match selected {
            Entry::Bin(id) => pending_bins.push((id, usize::from(id != root))),
            Entry::Item(id) => pending_items.push(id),
        }
        while let Some((id, depth)) = pending_bins.pop() {
            let key = Entry::Bin(id);
            let read = if depth > bins::MAX_DEPTH { Err(invalid("excessive bin depth")) } else { bins::read_bin(folder, &bins, id, &mut budget) };
            let (bin, raw) = match read {
                Ok(pair) => pair,
                Err(e) => {
                    result.errors.push((key, e.to_string()));
                    result.hierarchy_complete = false;
                    continue;
                }
            };
            result.records.insert(key, raw);
            for &entry in &bin.children {
                if result.parents.contains_key(&entry) || entry == Entry::Bin(root) {
                    result.ambiguous.insert(entry);
                    result.errors.push((entry, format!("repeated member or invalid hierarchy in bin {id}")));
                    result.hierarchy_complete = false;
                    continue;
                }
                result.parents.insert(entry, Some(id));
                match entry {
                    Entry::Bin(child) => pending_bins.push((child, depth + 1)),
                    Entry::Item(item) => pending_items.push(item),
                }
            }
            result.children.insert(id, bin.children);
        }
        let mut seen = BTreeSet::new();
        while let Some(id) = pending_items.pop() {
            if !seen.insert(id) {
                continue;
            }
            let key = Entry::Item(id);
            match recovery::read_item(folder, &items, &inventory, id, &mut budget) {
                Ok((item, raw)) => {
                    if matches!(selection,Selection::Sequence(seq) if id==seq) {
                        match super::sequence::Cut::read(&raw) {
                            Ok(cut) => result.cut = Some(cut),
                            Err(e) => {
                                result.errors.push((key, e.to_string()));
                                continue;
                            }
                        }
                    }
                    recovery::references(&item, &mut pending_items);
                    let mut metadata = serde_json::Map::new();
                    for name in recovery::SIDECARS {
                        if let Some(value) = envelope["project"][name].get(id.to_string()) {
                            metadata.insert(name.into(), value.clone());
                        }
                    }
                    if !metadata.is_empty() {
                        result.sidecars.insert(id, Value::Object(metadata));
                    }
                    result.records.insert(key, raw);
                }
                Err(e) => result.errors.push((key, e.to_string())),
            }
        }
        Ok(result)
    }
}
