//! Read one historical bin at a time without opening it for editing.
use super::{
    bins::{self, Entry},
    invalid, record_index, recovery,
};
use filmcraft_project::{Bin, BinEntry, BinId, ItemId, Project};
use std::{io, path::Path};

pub fn revisions(folder: &Path, bin: Option<u64>) -> io::Result<Vec<(u64, String)>> {
    let mut entries = Vec::new();
    recovery::walk(folder, |hash, m| {
        if bin.is_none_or(|id| m.bins.iter().any(|(key, _)| *key == id)) {
            entries.push((m.revision.as_ref().map_or(0, |r| r.number), hash.to_owned()));
        }
        Ok(false)
    })?;
    Ok(entries)
}

/// A shallow bin plus only the items needed to render its rows and reference clips.
pub fn projection(folder: &Path, revision: &str, bin: u64) -> io::Result<Project> {
    let m = recovery::at(folder, revision)?;
    let mut budget = crate::MAX_PROJECT_BYTES;
    let (env, _) = recovery::metadata(folder, &m, &mut budget)?;
    let index = record_index(&m.bins);
    let (saved, raw) = bins::read_bin(folder, &index, bin, &mut budget)?;
    let filed: Vec<ItemId> = raw.get("filed_sources").map(serde::Deserialize::deserialize).transpose()?.unwrap_or_default();
    let mut pending: Vec<_> = filed.iter().map(|id| id.0).collect();
    let mut children = Vec::new();
    for row in saved.children {
        match row {
            Entry::Item(id) => {
                pending.push(id);
                children.push(BinEntry::Item(ItemId(id)));
            }
            Entry::Bin(id) => {
                if id == bin {
                    return Err(invalid(format!("bin cycle at {id}")));
                }
                let (child, _) = bins::read_bin(folder, &index, id, &mut budget)?;
                children.push(BinEntry::Bin(Bin { id: BinId(id), name: child.name, children: Vec::new(), filed_sources: Vec::new() }));
            }
        }
    }
    let mut records = recovery::extract_items(folder, &m, env, &mut budget, pending)?;
    let root = Bin { id: BinId(bin), name: saved.name, children, filed_sources: filed };
    let mut document = records.envelope.take();
    let project = &mut document["project"];
    project["root"] = serde_json::to_value(root)?;
    project["items"] = serde_json::to_value(records.items)?;
    serde_json::from_value(project.take()).map_err(invalid)
}
