//! History is reachable from a single snapshot of the root. Rescue reads only the
//! envelope and the selected sequence's transitive project-item dependencies.
use super::{MANIFEST_LIMIT, Manifest, ROOT, check_hash, check_manifest, digest, encode_at, invalid, load_payload, read_member, read_records, revisions};
use crate::{MAX_PROJECT_BYTES, atomic_write_new};
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_project::{ItemId, Project, ProjectItem};
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
        if expected_number.is_some_and(|n| number != n) {
            return Err(invalid("broken project revision numbering"));
        }
        if visit(&hash, &manifest)? {
            return Ok(Some(manifest));
        }
        let Some(revision) = manifest.revision else { return Ok(None) };
        // check_manifest validated the parent digest and nonzero revision number.
        bytes = read_member(&folder.join(&revision.parent), MANIFEST_LIMIT)?;
        if digest(&bytes) != revision.parent {
            return Err(invalid(format!("damaged archived manifest {}", revision.parent)));
        }
        hash = revision.parent;
        expected_number = Some(number - 1);
    }
}

pub fn history(folder: &Path, mut visit: impl FnMut(u64, &str) -> io::Result<()>) -> io::Result<()> {
    let state = super::retention::Catalog::read(folder)?;
    walk(folder, |hash, m| {
        let n = m.revision.as_ref().map_or(0, |r| r.number);
        if state.available(n, hash) {
            visit(n, hash)?;
        }
        Ok(false)
    })?;
    Ok(())
}

pub fn move_backups(folder: &Path, mut visit: impl FnMut(u64, &super::MoveBackup) -> io::Result<()>) -> io::Result<()> {
    walk(folder, |_, manifest| {
        if let Some(rev) = &manifest.revision {
            for backup in &rev.moves {
                visit(rev.number, backup)?;
            }
        }
        Ok(false)
    })?;
    Ok(())
}

fn at(folder: &Path, requested: &str) -> io::Result<Manifest> {
    check_hash(requested)?;
    let state = super::retention::Catalog::read(folder)?;
    let mut departure = false;
    let manifest = walk(folder, |hash, m| {
        departure = m.revision.as_ref().is_some_and(|r| r.moves.iter().any(|b| b.snapshot == requested));
        Ok(departure || requested == hash && state.available(m.revision.as_ref().map_or(0, |r| r.number), hash))
    })?
    .ok_or_else(|| invalid("selected revision is outside the current project history"))?;
    if departure {
        let bytes = read_member(&folder.join(requested), MANIFEST_LIMIT)?;
        if digest(&bytes) != requested {
            return Err(invalid("damaged sequence move backup"));
        }
        let saved = serde_json::from_slice(&bytes)?;
        check_manifest(&saved)?;
        Ok(saved)
    } else {
        Ok(manifest)
    }
}

fn extract(folder: &Path, manifest: &Manifest, sequence: u64) -> io::Result<ProjectRecords> {
    let mut budget = MAX_PROJECT_BYTES;
    let mut envelope = load_payload(folder, &manifest.envelope, &mut budget)?;
    let project = envelope.get_mut("project").and_then(Value::as_object_mut).ok_or_else(|| invalid("missing project metadata"))?;
    let root = project.get("root").and_then(Value::as_u64).ok_or_else(|| invalid("missing root bin id"))?;
    let inventory: Vec<u64> = serde_json::from_value(project.get("items").ok_or_else(|| invalid("missing item inventory"))?.clone())?;
    let inventory_len = inventory.len();
    let inventory: BTreeSet<_> = inventory.into_iter().collect();
    if inventory.len() != inventory_len {
        return Err(invalid("duplicate item in project inventory"));
    }
    let hashes: BTreeMap<_, _> = manifest.items.iter().map(|(id, hash)| (*id, hash)).collect();
    if hashes.len() != manifest.items.len() {
        return Err(invalid("duplicate item in manifest"));
    }
    let mut items = BTreeMap::new();
    let mut pending = vec![sequence];
    while let Some(id) = pending.pop() {
        if items.contains_key(&id) {
            continue;
        }
        let hash = hashes.get(&id).filter(|_| inventory.contains(&id)).ok_or_else(|| invalid(format!("missing item {id}")))?;
        let mut value = load_payload(folder, hash, &mut budget).map_err(|e| invalid(format!("item {id}: {e}")))?;
        if let Some(identity) = manifest.identities.get(&id) {
            identity.apply(id, &mut value)?;
        }
        let item: ProjectItem = serde_json::from_value(value.clone()).map_err(|e| invalid(format!("item {id}: {e}")))?;
        if item.id.0 != id {
            return Err(invalid(format!("mismatched item {id}")));
        }
        if id == sequence && item.as_sequence().is_none() {
            return Err(invalid(format!("item {id} is not a sequence")));
        }
        item.visit_source_items(|source| pending.push(source.0));
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
    envelope["view"] = json!({"open_sequences":[sequence], "active_sequence":sequence});
    let entries: Vec<_> = ids.iter().map(|id| json!({"Item":id})).collect();
    let bin = json!({"id":root,"name":"Recovered","children":entries});
    Ok(ProjectRecords { envelope, bins: BTreeMap::from([(root, bin)]), items })
}

pub(super) fn verify_cut(folder: &Path, hash: &str, sequence: u64) -> io::Result<()> {
    let cut = extract(folder, &at(folder, hash)?, sequence)?;
    encode_at(folder, cut, &folder.join(ROOT)).map(|_| ())
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
    let present = live.envelope.get_mut("project").filter(|value| value.is_object()).ok_or_else(|| invalid("missing current project metadata"))?;
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
