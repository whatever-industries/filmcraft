//! History is reachable from a single snapshot of the root. Rescue reads only the
//! envelope and the selected sequence's transitive project-item dependencies.
use super::{
    MANIFEST_LIMIT, Manifest, ROOT, check_hash, check_manifest, digest, encode_at, invalid, load_payload, read_member, read_records, record_index, revisions,
};
use crate::{MAX_PROJECT_BYTES, atomic_write_new};
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_project::{ItemId, ItemKind, Project, ProjectItem};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::{io, path::Path};

pub(super) const SIDECARS: [&str; 3] = ["transcripts", "source_graphics", "narrations"];

pub(super) fn walk(folder: &Path, mut visit: impl FnMut(&str, &Manifest) -> io::Result<bool>) -> io::Result<Option<Manifest>> {
    let mut bytes = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
    let mut hash = digest(&bytes);
    let mut expected_number = None;
    loop {
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        check_manifest(&manifest)?;
        let number = manifest.revision.as_ref().map_or(0, |r| r.number);
        if expected_number.is_some_and(|n| number != n) || (number == 0 && manifest.revision.is_some()) {
            return Err(invalid("broken project revision numbering"));
        }
        if visit(&hash, &manifest)? {
            return Ok(Some(manifest));
        }
        let Some(revision) = manifest.revision else { return Ok(None) };
        check_hash(&revision.parent)?;
        bytes = read_member(&folder.join(&revision.parent), MANIFEST_LIMIT)?;
        if digest(&bytes) != revision.parent {
            return Err(invalid(format!("damaged archived manifest {}", revision.parent)));
        }
        hash = revision.parent;
        expected_number = Some(number - 1);
    }
}

pub fn history(folder: &Path, mut visit: impl FnMut(u64, &str) -> io::Result<()>) -> io::Result<()> {
    walk(folder, |hash, m| {
        visit(m.revision.as_ref().map_or(0, |r| r.number), hash)?;
        Ok(false)
    })?;
    Ok(())
}

pub(super) fn at(folder: &Path, requested: &str) -> io::Result<Manifest> {
    check_hash(requested)?;
    walk(folder, |hash, _| Ok(requested == hash))?.ok_or_else(|| invalid("selected revision is outside the current project history"))
}

pub(super) fn references(item: &ProjectItem, pending: &mut Vec<u64>) {
    match &item.kind {
        ItemKind::Subclip { parent, .. } => pending.push(parent.0),
        ItemKind::Sequence(seq) => {
            pending.extend(seq.all_tracks().chain(&seq.submix_tracks).flat_map(|track| &track.items).map(|clip| clip.item.0));
            pending.extend(seq.multicam.iter().flat_map(|m| &m.cameras).filter_map(|c| c.source).map(|id| id.0));
            pending.extend(seq.merged.iter().flat_map(|m| m.video.iter().chain(&m.audio)).map(|id| id.0));
        }
        ItemKind::Media(_) | ItemKind::Graphic { .. } | ItemKind::AdjustmentLayer { .. } => {}
    }
}

pub(super) fn metadata(folder: &Path, manifest: &Manifest, budget: &mut u64) -> io::Result<(Value, u64)> {
    let envelope = load_payload(folder, &manifest.envelope, budget).map_err(|e| invalid(format!("project metadata: {e}")))?;
    craft_fcproj_format::detect_version(&envelope).and_then(craft_fcproj_format::check_supported).map_err(invalid)?;
    let root = envelope.pointer("/project/root").and_then(Value::as_u64).ok_or_else(|| invalid("missing root bin id"))?;
    Ok((envelope, root))
}

pub(super) fn inventory(value: Option<&Value>) -> io::Result<BTreeSet<u64>> {
    let inventory: Vec<u64> = serde_json::from_value(value.ok_or_else(|| invalid("missing item inventory"))?.clone())?;
    let inventory_len = inventory.len();
    let inventory: BTreeSet<_> = inventory.into_iter().collect();
    if inventory.len() != inventory_len {
        return Err(invalid("duplicate item in project inventory"));
    }
    Ok(inventory)
}

pub(super) fn read_item(
    folder: &Path,
    hashes: &super::RecordIndex<'_>,
    inventory: &BTreeSet<u64>,
    id: u64,
    budget: &mut u64,
) -> io::Result<(ProjectItem, Value)> {
    let hash = hashes.get(&id).copied().flatten().filter(|_| inventory.contains(&id)).ok_or_else(|| invalid(format!("missing or repeated item {id}")))?;
    let value = load_payload(folder, hash, budget).map_err(|e| invalid(format!("item {id}: {e}")))?;
    let item: ProjectItem = serde::Deserialize::deserialize(&value).map_err(|e| invalid(format!("item {id}: {e}")))?;
    if item.id.0 != id {
        return Err(invalid(format!("mismatched item {id}")));
    }
    Ok((item, value))
}

pub(super) fn extract_items(folder: &Path, manifest: &Manifest, mut envelope: Value, budget: &mut u64, mut pending: Vec<u64>) -> io::Result<ProjectRecords> {
    let project = envelope.get_mut("project").and_then(Value::as_object_mut).ok_or_else(|| invalid("missing project metadata"))?;
    let inventory = inventory(project.get("items"))?;
    let hashes = record_index(&manifest.items);
    let mut items = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if items.contains_key(&id) {
            continue;
        }
        let (item, value) = read_item(folder, &hashes, &inventory, id, budget)?;
        references(&item, &mut pending);
        items.insert(id, value);
    }
    for key in SIDECARS {
        if let Some(map) = project.get_mut(key).and_then(Value::as_object_mut) {
            map.retain(|id, _| id.parse::<u64>().is_ok_and(|id| items.contains_key(&id)));
        }
    }
    let ids: Vec<_> = items.keys().copied().collect();
    project.insert("items".into(), json!(ids));
    project.remove("search_bins");
    envelope.as_object_mut().ok_or_else(|| invalid("missing envelope"))?.remove("view");
    Ok(ProjectRecords { envelope, bins: BTreeMap::new(), items })
}

fn extract(folder: &Path, manifest: &Manifest, sequence: u64) -> io::Result<ProjectRecords> {
    let mut budget = MAX_PROJECT_BYTES;
    let (envelope, root) = metadata(folder, manifest, &mut budget)?;
    let mut records = extract_items(folder, manifest, envelope, &mut budget, vec![sequence])?;
    if !records.items.get(&sequence).is_some_and(|item| item.pointer("/kind/Sequence").is_some()) {
        return Err(invalid(format!("item {sequence} is not a sequence")));
    }
    records.envelope["view"] = json!({"open_sequences":[sequence], "active_sequence":sequence});
    let entries: Vec<_> = records.items.keys().map(|id| json!({"Item":id})).collect();
    records.bins.insert(root, json!({"id":root,"name":"Recovered","children":entries}));
    Ok(records)
}

pub fn rescue(folder: &Path, revision: &str, sequence: u64, output: &Path) -> io::Result<()> {
    let records = extract(folder, &at(folder, revision)?, sequence)?;
    atomic_write_new(output, &encode_at(folder, records, output)?)
}

pub fn restore_copy(folder: &Path, expected: &str, historical: &str, sequence: u64) -> io::Result<(u64, String)> {
    let root = folder.join(ROOT);
    let current = read_member(&root, MANIFEST_LIMIT)?;
    if digest(&current) != expected {
        return Err(invalid("stale project revision"));
    }
    let manifest = serde_json::from_slice(&current)?;
    let mut live = read_records(folder, &manifest)?;
    let mut recovered = extract(folder, &at(folder, historical)?, sequence)?;
    let past = &recovered.envelope["project"];
    let present = &mut live.envelope["project"];
    if past.get("luts") != present.get("luts") {
        return Err(invalid("project LUT library changed; use rescue to export the historical sequence"));
    }
    for (&id, value) in &recovered.items {
        if id == sequence {
            continue;
        }
        let key = id.to_string();
        if live.items.get(&id) != Some(value) || SIDECARS.iter().any(|field| past[field].get(&key) != present[field].get(&key)) {
            return Err(invalid(format!("dependency item {id} changed or is missing; use rescue to export the historical sequence")));
        }
    }
    let project = crate::decode(&super::assemble(folder, &manifest, &root)?).map_err(invalid)?.project;
    // The standard reader repairs low next_id values. Include this historical cut's
    // internal IDs too, even if they were deleted from the live project.
    let mut item = recovered.items.remove(&sequence).ok_or_else(|| invalid("missing selected sequence"))?;
    let old_project_next = past["next_id"].as_u64().ok_or_else(|| invalid("missing historical id counter"))?;
    let mut historical_cut = Project::new("");
    historical_cut.items.insert(ItemId(sequence), serde_json::from_value(item.clone())?);
    let id = project.next_id.max(old_project_next).max(historical_cut.max_id().saturating_add(1));
    if id >= Project::MAX_NEXT_ID {
        return Err(invalid("project id limit reached"));
    }
    item["id"] = json!(id);
    item["created"] = json!(id);
    item["name"] = json!(format!("{} (Recovered)", item["name"].as_str().ok_or_else(|| invalid("missing sequence name"))?));
    live.items.insert(id, item);
    present["items"].as_array_mut().ok_or_else(|| invalid("missing inventory"))?.push(json!(id));
    present["next_id"] = json!(id + 1);
    for field in SIDECARS {
        if let Some(value) = past[field].get(sequence.to_string()) {
            present[field][id.to_string()] = value.clone();
        }
    }
    let bin_id = project.root.parent_of(ItemId(sequence)).unwrap_or(project.root.id);
    let bin = live.bins.get_mut(&bin_id.0).ok_or_else(|| invalid("missing destination bin"))?;
    bin["children"].as_array_mut().ok_or_else(|| invalid("missing bin children"))?.push(json!({"Item":id}));
    Ok((id, revisions::save_with(folder, expected, live, revisions::publish)?))
}
