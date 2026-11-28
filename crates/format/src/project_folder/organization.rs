//! Project the modular directory into ordinary nested bins for a portable document.
use super::invalid;
use filmcraft_project::{BinId, DirectoryEntry, Project, ProjectDirectory};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

fn collect(mut bin: Value, records: &mut BTreeMap<BinId, Value>) -> io::Result<()> {
    let id = BinId(bin["id"].as_u64().ok_or_else(|| invalid("missing bin ID"))?);
    let children = bin["children"].as_array_mut().ok_or_else(|| invalid("missing bin entries"))?;
    let mut items = Vec::new();
    for mut entry in std::mem::take(children) {
        if let Some(child) = entry.get_mut("Bin") {
            collect(child.take(), records)?;
        } else {
            items.push(entry);
        }
    }
    *children = items;
    if records.insert(id, bin).is_some() {
        return Err(invalid("repeated bin ID"));
    }
    Ok(())
}

fn folder(id: BinId, dir: &ProjectDirectory, records: &BTreeMap<BinId, Value>, bins: &BTreeSet<BinId>, next: &mut u64) -> io::Result<Value> {
    let f = dir.folders.get(&id).ok_or_else(|| invalid("unknown folder"))?;
    let mut raw = records.get(&id).cloned().unwrap_or_else(|| json!({}));
    let portable = if bins.contains(&id) {
        if *next >= Project::MAX_NEXT_ID {
            return Err(invalid("project ID limit reached"));
        }
        let allocated = *next;
        *next += 1;
        allocated
    } else {
        id.0
    };
    let children = f
        .children
        .iter()
        .map(|entry| {
            let child = match *entry {
                DirectoryEntry::Folder(child) => folder(child, dir, records, bins, next)?,
                DirectoryEntry::Bin(bin) => records.get(&bin).cloned().ok_or_else(|| invalid("unknown editorial bin"))?,
            };
            Ok(json!({"Bin":child}))
        })
        .collect::<io::Result<Vec<_>>>()?;
    raw["id"] = json!(portable);
    raw["name"] = json!(f.name);
    raw["children"] = json!(children);
    Ok(raw)
}

pub(super) fn portable(project: &mut Value) -> io::Result<()> {
    let Some(raw) = project.as_object_mut().and_then(|p| p.remove("modular_directory")) else { return Ok(()) };
    let directory: ProjectDirectory = serde_json::from_value(raw)?;
    let mut records = BTreeMap::new();
    collect(project["root"].take(), &mut records)?;
    directory.validate(&records.keys().copied().collect()).map_err(invalid)?;
    let bins = directory
        .folders
        .values()
        .flat_map(|f| &f.children)
        .filter_map(|entry| match entry {
            DirectoryEntry::Bin(id) => Some(*id),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if records.iter().any(|(id, raw)| !bins.contains(id) && raw["children"].as_array().is_some_and(|c| !c.is_empty())) {
        return Err(invalid("directory hides filed items"));
    }
    let mut next = project["next_id"].as_u64().ok_or_else(|| invalid("missing project ID counter"))?;
    project["root"] = folder(directory.root, &directory, &records, &bins, &mut next)?;
    project["next_id"] = json!(next);
    Ok(())
}
