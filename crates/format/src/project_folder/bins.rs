//! Read a single revision's bin hierarchy. Rescue walks outward from the requested
//! bin, so an unreadable sibling (or parent) cannot prevent an independent export.
use super::{RECORD_LIMIT, RecordIndex, checked_bytes, document_at, invalid, load_payload, record_index, recovery};
use crate::{MAX_PROJECT_BYTES, atomic_write_new};
use filmcraft_project::Project;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;

// The shared record codec caps reassembled bin nesting at 64.
const MAX_DEPTH: usize = 64;

#[derive(Deserialize)]
enum Entry {
    Bin(u64),
    Item(u64),
}

#[derive(Deserialize)]
struct Bin {
    id: u64,
    name: String,
    children: Vec<Entry>,
}

fn read_bin(folder: &Path, ids: &RecordIndex<'_>, id: u64, budget: &mut u64) -> io::Result<(Bin, Value)> {
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

pub fn list_bins(folder: &Path, revision: &str, mut out: impl Write) -> io::Result<()> {
    let manifest = recovery::at(folder, revision)?;
    let mut budget = MAX_PROJECT_BYTES;
    let (_, root) = recovery::metadata(folder, &manifest, &mut budget)?;
    let ids = record_index(&manifest.bins);
    let mut names = BTreeMap::new();
    let mut errors = BTreeMap::new();
    let mut parents = BTreeMap::new();
    if !ids.contains_key(&root) {
        errors.insert(root, format!("missing root bin {root}"));
    }
    for &id in ids.keys() {
        match read_bin(folder, &ids, id, &mut budget) {
            Ok((bin, _)) => {
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
    for (&id, name) in &names {
        let path = path_to(id, root, &names, &parents);
        incomplete |= path.is_none();
        serde_json::to_writer(&mut out, &json!({"id":id,"name":name,"path":path}))?;
        writeln!(out)?;
    }
    for (id, error) in errors {
        serde_json::to_writer(&mut out, &json!({"id":id,"error":error}))?;
        writeln!(out)?;
    }
    if incomplete {
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

pub fn rescue_bin(folder: &Path, revision: &str, selected: u64, output: &Path) -> io::Result<()> {
    super::outside_folder(folder, output)?;
    let manifest = recovery::at(folder, revision)?;
    let mut budget = MAX_PROJECT_BYTES;
    let (envelope, root) = recovery::metadata(folder, &manifest, &mut budget)?;
    let ids = record_index(&manifest.bins);
    let mut bins = BTreeMap::new();
    let mut members = BTreeSet::new();
    let mut pending = vec![(selected, usize::from(selected != root))];
    while let Some((id, depth)) = pending.pop() {
        if depth > MAX_DEPTH || bins.contains_key(&id) || (id == root && selected != root) {
            return Err(invalid(format!("invalid bin hierarchy at bin {id} (repeated bin or excessive depth)")));
        }
        let (bin, value) = read_bin(folder, &ids, id, &mut budget)?;
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
    if selected != root {
        let name = envelope.pointer("/project/name").and_then(Value::as_str).ok_or_else(|| invalid("missing project name"))?;
        bins.insert(root, json!({"id":root,"name":name,"children":[{"Bin":selected}]}));
    }
    let mut records = recovery::extract_items(folder, &manifest, envelope, &mut budget, members.iter().copied().collect())?;
    let external: Vec<_> = records.items.keys().filter(|id| !members.contains(id)).map(|id| json!({"Item":id})).collect();
    if bins.len() + records.items.len() + usize::from(!external.is_empty()) > RECORD_LIMIT {
        return Err(invalid("recovered project exceeds the record limit"));
    }
    records.bins = bins;
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
    atomic_write_new(output, &checked_bytes(&doc)?)
}
