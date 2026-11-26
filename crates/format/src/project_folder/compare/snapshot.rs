//! Read the selected hierarchy and its dependency closure, retaining verified siblings on error.
use super::Selection;
use crate::MAX_PROJECT_BYTES;
use crate::project_folder::{
    Manifest,
    bins::{self, Entry},
    digest, invalid, record_index, recovery,
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
    pub filings: BTreeMap<u64, BTreeSet<u64>>,
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
            if let Some(links) = raw.get("filed_sources") {
                match serde_json::from_value::<Vec<u64>>(links.clone()) {
                    Ok(ids) => {
                        for item in ids {
                            result.filings.entry(item).or_default().insert(id);
                            pending_items.push(item);
                        }
                    }
                    Err(e) => {
                        result.errors.push((key, e.to_string()));
                        result.hierarchy_complete = false;
                    }
                }
            }
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
        let library: BTreeMap<_, _> = envelope["project"]["luts"].as_array().into_iter().flatten().filter_map(|lut| Some((lut["id"].as_str()?, lut))).collect();
        let mut lut_summaries = BTreeMap::new();
        let mut seen = BTreeSet::new();
        while let Some(id) = pending_items.pop() {
            if !seen.insert(id) {
                continue;
            }
            let key = Entry::Item(id);
            match recovery::read_item(folder, &items, &inventory, id, &mut budget) {
                Ok((item, mut raw)) => {
                    if let Some(obj) = raw.as_object_mut() {
                        obj.remove("field_edits");
                        obj.remove("modular_history_origin");
                    }
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
                    let mut used = BTreeSet::new();
                    for value in std::iter::once(&mut raw).chain(metadata.values_mut()) {
                        super::super::history_luts::referenced(value, &mut used)?;
                    }
                    for id in used {
                        if let Some(lut) = library.get(id.as_str()) {
                            // Many cuts can use the same large LUT. Hash its text once per snapshot.
                            let summary = lut_summaries.entry(id.clone()).or_insert_with(|| {
                                let mut value = (*lut).clone();
                                if let Some(text) = value["text"].as_str().filter(|s| s.len() > 240) {
                                    value["text"] = serde_json::json!({"bytes":text.len(),"sha256":digest(text.as_bytes())});
                                }
                                value
                            });
                            metadata.entry("luts").or_insert_with(|| serde_json::json!({}))[&id] = summary.clone();
                        } else {
                            result.errors.push((key, format!("missing referenced LUT {id}")));
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
