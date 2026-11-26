//! Read a single revision's bin hierarchy. Rescue walks outward from the requested
//! bin, so an unreadable sibling (or parent) cannot prevent an independent export.
use super::{Manifest, ProjectRecords, RECORD_LIMIT, RecordIndex, checked_bytes, document_at, invalid, load_payload, record_index, recovery};
use crate::{MAX_PROJECT_BYTES, atomic_write_new};
use filmcraft_project::Project;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;

// The shared record codec caps reassembled bin nesting at 64.
pub(super) const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Entry {
    Bin(u64),
    Item(u64),
}

#[derive(Deserialize)]
pub(super) struct Bin {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) children: Vec<Entry>,
}

pub(super) fn read_bin(folder: &Path, ids: &RecordIndex<'_>, id: u64, budget: &mut u64) -> io::Result<(Bin, Value)> {
    let hash = ids.get(&id).copied().flatten().ok_or_else(|| invalid(format!("missing or repeated bin {id}")))?;
    let value = load_payload(folder, hash, budget).map_err(|e| invalid(format!("bin {id}: {e}")))?;
    let bin = Bin::deserialize(&value).map_err(|e| invalid(format!("bin {id}: {e}")))?;
    if bin.id != id {
        return Err(invalid(format!("mismatched bin {id}")));
    }
    Ok((bin, value))
}

fn path_to<'a>(mut id: u64, root: u64, names: &'a BTreeMap<u64, String>, parents: &BTreeMap<u64, Option<u64>>) -> Option<Vec<&'a str>> {
    let mut path = Vec::new();
    for _ in 0..=MAX_DEPTH {
        path.push(names.get(&id)?.as_str());
        if id == root {
            if parents.contains_key(&root) {
                return None;
            }
            path.reverse();
            return Some(path);
        }
        id = parents.get(&id).copied().flatten()?;
    }
    None
}

#[derive(serde::Serialize)]
pub struct BinListing {
    pub bins: Vec<BinLocation>,
    pub errors: BTreeMap<u64, String>,
    pub incomplete: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct BinLocation {
    pub id: u64,
    pub name: String,
    pub path: Option<Vec<String>>,
}

pub fn saved_bins(folder: &Path, revision: &str) -> io::Result<BinListing> {
    let manifest = recovery::at(folder, revision)?;
    let mut budget = MAX_PROJECT_BYTES;
    let (_, root) = recovery::metadata(folder, &manifest, &mut budget)?;
    Ok(scan_bins(folder, &manifest, root, &mut budget, |_, _| Ok(())))
}

pub(super) fn scan_bins(
    folder: &Path,
    manifest: &super::Manifest,
    root: u64,
    budget: &mut u64,
    mut visit: impl FnMut(&Bin, &Value) -> io::Result<()>,
) -> BinListing {
    let ids = record_index(&manifest.bins);
    let mut names = BTreeMap::new();
    let mut errors = BTreeMap::new();
    let mut parents = BTreeMap::new();
    if !ids.contains_key(&root) {
        errors.insert(root, format!("missing root bin {root}"));
    }
    for &id in ids.keys() {
        match read_bin(folder, &ids, id, budget) {
            Ok((bin, raw)) => {
                if let Err(e) = visit(&bin, &raw) {
                    errors.insert(id, e.to_string());
                }
                names.insert(id, bin.name);
                for entry in bin.children {
                    if let Entry::Bin(child) = entry {
                        parents.entry(child).and_modify(|p| *p = None).or_insert(Some(id));
                        if !ids.contains_key(&child) {
                            errors.insert(child, format!("missing bin {child} referenced by bin {id}"));
                        }
                    }
                }
            }
            Err(e) => {
                errors.insert(id, e.to_string());
            }
        }
    }
    let mut incomplete = !errors.is_empty();
    let bins = names
        .iter()
        .map(|(&id, name)| {
            let path = path_to(id, root, &names, &parents);
            incomplete |= path.is_none();
            BinLocation { id, name: name.clone(), path: path.map(|parts| parts.into_iter().map(str::to_owned).collect()) }
        })
        .collect();
    BinListing { bins, errors, incomplete }
}

pub fn list_bins(folder: &Path, revision: &str, mut out: impl Write) -> io::Result<()> {
    let listing = saved_bins(folder, revision)?;
    for bin in listing.bins {
        serde_json::to_writer(&mut out, &bin)?;
        writeln!(out)?;
    }
    for (id, error) in listing.errors {
        serde_json::to_writer(&mut out, &json!({"id":id,"error":error}))?;
        writeln!(out)?;
    }
    if listing.incomplete {
        return Err(invalid("incomplete bin listing; some bins or paths could not be verified"));
    }
    Ok(())
}

fn attachment_name(root: &Value, items: &Value) -> io::Result<String> {
    let entries = root["children"].as_array().ok_or_else(|| invalid("missing recovered root children"))?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let record = if let Some(id) = entry["Item"].as_u64() { &items[id.to_string()] } else { &entry["Bin"] };
        if let Some(name) = record["name"].as_str() {
            names.insert(name);
        }
    }
    let base = "Referenced Items";
    if !names.contains(base) {
        return Ok(base.to_owned());
    }
    for n in 2..=entries.len() + 2 {
        let candidate = format!("{base} {n}");
        if !names.contains(candidate.as_str()) {
            return Ok(candidate);
        }
    }
    Err(invalid("no available bin name"))
}

struct BinSubset {
    root: u64,
    records: ProjectRecords,
    filed: BTreeSet<u64>,
}

fn read_subset(folder: &Path, manifest: &Manifest, selected: u64) -> io::Result<BinSubset> {
    let mut budget = MAX_PROJECT_BYTES;
    let (envelope, root) = recovery::metadata(folder, manifest, &mut budget)?;
    let ids = record_index(&manifest.bins);
    let mut bins = BTreeMap::new();
    let mut members = BTreeSet::new();
    let mut pending = vec![(selected, usize::from(selected != root))];
    while let Some((id, depth)) = pending.pop() {
        if depth > MAX_DEPTH || bins.contains_key(&id) || (id == root && selected != root) {
            return Err(invalid(format!("invalid bin hierarchy at bin {id} (repeated bin or excessive depth)")));
        }
        let (bin, value) = read_bin(folder, &ids, id, &mut budget)?;
        if let Some(extra) = value.get("filed_sources") {
            members.extend(serde_json::from_value::<Vec<u64>>(extra.clone())?);
        }
        for child in bin.children {
            match child {
                Entry::Bin(id) => pending.push((id, depth + 1)),
                Entry::Item(id) => {
                    members.insert(id);
                }
            }
        }
        bins.insert(id, value);
    }
    let mut records = recovery::extract_items(folder, manifest, envelope, &mut budget, members.iter().copied().collect())?;
    records.bins = bins;
    Ok(BinSubset { root, records, filed: members })
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct NamedRecord {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, serde::Serialize)]
pub struct BinPreview {
    pub revision: String,
    pub selected: u64,
    pub bins: Vec<NamedRecord>,
    pub filed_items: Vec<NamedRecord>,
    pub referenced_items: Vec<NamedRecord>,
}

/// Inventory the selected saved subtree and the additional items its contents need.
pub fn preview_bin(folder: &Path, revision: &str, selected: u64) -> io::Result<BinPreview> {
    let manifest = recovery::at(folder, revision)?;
    let BinSubset { records, filed, .. } = read_subset(folder, &manifest, selected)?;
    let bins = records.bins.values().map(NamedRecord::deserialize).collect::<Result<_, _>>()?;
    let mut filed_items = Vec::new();
    let mut referenced_items = Vec::new();
    for (id, item) in records.items {
        let group = if filed.contains(&id) { &mut filed_items } else { &mut referenced_items };
        group.push(NamedRecord::deserialize(item)?);
    }
    Ok(BinPreview { revision: revision.to_owned(), selected, bins, filed_items, referenced_items })
}

pub fn rescue_bin(folder: &Path, revision: &str, selected: u64, output: &Path) -> io::Result<()> {
    super::outside_folder(folder, output)?;
    let manifest = recovery::at(folder, revision)?;
    let BinSubset { root, mut records, filed } = read_subset(folder, &manifest, selected)?;
    if selected != root {
        let name = records.envelope.pointer("/project/name").and_then(Value::as_str).ok_or_else(|| invalid("missing project name"))?;
        records.bins.insert(root, json!({"id":root,"name":name,"children":[{"Bin":selected}]}));
    }
    let external: Vec<_> = records.items.keys().filter(|id| !filed.contains(id)).map(|id| json!({"Item":id})).collect();
    if records.bins.len() + records.items.len() + usize::from(!external.is_empty()) > RECORD_LIMIT {
        return Err(invalid("recovered project exceeds the record limit"));
    }
    let mut doc = document_at(folder, records, output)?;
    if !external.is_empty() {
        // The stock reader normalizes stale next_id using every retained track,
        // clip and sidecar. Also reserve IDs of original records omitted here.
        let mut id = crate::decode(&serde_json::to_vec(&doc)?).map_err(invalid)?.project.next_id;
        let used: BTreeSet<_> = manifest.items.iter().chain(&manifest.bins).map(|(id, _)| *id).collect();
        while used.contains(&id) && id < Project::MAX_NEXT_ID {
            id += 1;
        }
        if id >= Project::MAX_NEXT_ID {
            return Err(invalid("project id limit reached"));
        }
        let project = &mut doc["project"];
        let name = attachment_name(&project["root"], &project["items"])?;
        project["root"]["children"]
            .as_array_mut()
            .ok_or_else(|| invalid("missing root bin children"))?
            .push(json!({"Bin":{"id":id,"name":name,"children":external}}));
        project["next_id"] = json!(id + 1);
    }
    atomic_write_new(output, &super::standard_document(&checked_bytes(&doc)?)?)
}
