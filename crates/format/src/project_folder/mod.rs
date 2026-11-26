//! Native project-folder storage, editing and recovery. See docs/project-files.md.
use crate::{MAX_PROJECT_BYTES, atomic_write_new};
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_media::reader::read_regular_file_bounded;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub mod bin_history;
pub mod bins;
pub mod compare;
pub mod exchange;
mod history_luts;
pub mod history_paste;
mod paths;
pub mod recovery;
pub mod revisions;
pub mod usage;

pub const ROOT: &str = "project.fcmod";
const FORMAT: &str = "filmcraft.project-folder";
const MANIFEST_LIMIT: u64 = 16 << 20;
const RECORD_LIMIT: usize = 100_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    layout: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<revisions::Revision>,
    envelope: String,
    bins: Vec<(u64, String)>,
    items: Vec<(u64, String)>,
}

fn invalid(reason: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.to_string())
}

fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn read_member(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid(format!("expected a regular file at {}", path.display())));
    }
    read_regular_file_bounded(path, limit)
}

fn check_hash(hash: &str) -> io::Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(invalid("invalid SHA-256 digest"));
    }
    Ok(())
}

fn load_payload(folder: &Path, hash: &str, remaining: &mut u64) -> io::Result<Value> {
    check_hash(hash)?;
    let bytes = read_member(&folder.join(hash), *remaining)?;
    *remaining -= bytes.len() as u64;
    if digest(&bytes) != hash {
        return Err(invalid("SHA-256 mismatch"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) type RecordIndex<'a> = BTreeMap<u64, Option<&'a str>>;

pub(super) fn record_index(entries: &[(u64, String)]) -> RecordIndex<'_> {
    let mut ids = BTreeMap::new();
    for (id, hash) in entries {
        ids.entry(*id).and_modify(|entry| *entry = None).or_insert(Some(hash.as_str()));
    }
    ids
}

fn load_group(folder: &Path, kind: &str, entries: &[(u64, String)], budget: &mut u64) -> io::Result<BTreeMap<u64, Value>> {
    let mut group = BTreeMap::new();
    for (id, hash) in entries {
        let data = load_payload(folder, hash, budget).map_err(|e| invalid(format!("{kind} {id}: {e}")))?;
        if data.get("id").and_then(Value::as_u64) != Some(*id) || group.insert(*id, data).is_some() {
            return Err(invalid(format!("mismatched or repeated {kind} {id}")));
        }
    }
    Ok(group)
}

fn check_manifest(manifest: &Manifest) -> io::Result<()> {
    if manifest.format != FORMAT || !matches!((manifest.layout, &manifest.revision), (1, None) | (2, Some(_))) {
        return Err(invalid(format!("unsupported project folder format {:?}, layout {}", manifest.format, manifest.layout)));
    }
    if manifest.bins.len().saturating_add(manifest.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    Ok(())
}

fn read_records(folder: &Path, manifest: &Manifest) -> io::Result<ProjectRecords> {
    check_manifest(manifest)?;
    let mut budget = MAX_PROJECT_BYTES;
    let envelope = load_payload(folder, &manifest.envelope, &mut budget).map_err(|e| invalid(format!("project metadata: {e}")))?;
    let bins = load_group(folder, "bin", &manifest.bins, &mut budget)?;
    let items = load_group(folder, "item", &manifest.items, &mut budget)?;
    Ok(ProjectRecords { envelope, bins, items })
}

fn assemble(folder: &Path, manifest: &Manifest, destination: &Path) -> io::Result<Vec<u8>> {
    encode_at(folder, read_records(folder, manifest)?, destination)
}

fn document_at(folder: &Path, mut records: ProjectRecords, destination: &Path) -> io::Result<Value> {
    paths::rebase(&mut records, &folder.join(ROOT), destination)?;
    records.into_document().map_err(invalid)
}

fn checked_bytes(document: &Value) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(document)?;
    crate::decode(&bytes).map_err(invalid)?;
    Ok(bytes)
}

/// Convert the modular bin memberships and edit clock to an ordinary single-filing document.
pub fn standard_document(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut doc: Value = serde_json::from_slice(bytes)?;
    if let Some(items) = doc["project"]["items"].as_object_mut() {
        for item in items.values_mut().filter_map(Value::as_object_mut) {
            item.remove("field_edits");
            item.remove("modular_history_origin");
        }
    }
    // Keep home rows when available. A rescued subtree may contain only a secondary filing.
    let mut filed = std::collections::BTreeSet::new();
    let mut pending = vec![&doc["project"]["root"]];
    while let Some(bin) = pending.pop() {
        for entry in bin["children"].as_array().into_iter().flatten() {
            if let Some(id) = entry["Item"].as_u64() {
                filed.insert(id);
            }
            if let Some(child) = entry.get("Bin") {
                pending.push(child);
            }
        }
    }
    fn clean(bin: &mut Value, filed: &mut std::collections::BTreeSet<u64>) -> io::Result<()> {
        if let Some(links) = bin.as_object_mut().and_then(|b| b.remove("filed_sources")) {
            let ids: Vec<u64> = serde_json::from_value(links)?;
            let children = bin["children"].as_array_mut().ok_or_else(|| invalid("expected bin children"))?;
            children.extend(ids.into_iter().filter(|id| filed.insert(*id)).map(|id| serde_json::json!({"Item":id})));
        }
        if let Some(children) = bin["children"].as_array_mut() {
            for child in children.iter_mut().filter_map(|v| v.get_mut("Bin")) {
                clean(child, filed)?;
            }
        }
        Ok(())
    }
    clean(&mut doc["project"]["root"], &mut filed)?;
    checked_bytes(&doc)
}

fn encode_at(folder: &Path, records: ProjectRecords, destination: &Path) -> io::Result<Vec<u8>> {
    checked_bytes(&document_at(folder, records, destination)?)
}

fn write_payload(folder: &Path, value: &Value, write_new: &mut impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let hash = digest(&bytes);
    let path = folder.join(&hash);
    match write_new(&path, &bytes) {
        Ok(()) => (),
        // Equal content may share a payload. assemble verifies every referenced byte before publication.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    Ok(hash)
}

fn records_from(source: &Path, folder: &Path) -> io::Result<ProjectRecords> {
    let mut records = {
        let bytes = read_regular_file_bounded(source, MAX_PROJECT_BYTES)?;
        crate::decode(&bytes).map_err(invalid)?;
        ProjectRecords::split(&bytes).map_err(invalid)?
    };
    if records.bins.len().saturating_add(records.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    let root = folder.join(ROOT);
    paths::rebase(&mut records, source, &root)?;
    Ok(records)
}

type PriorGroup<'a> = (&'a [(u64, String)], &'a BTreeMap<u64, Value>);

fn materialize(
    folder: &Path,
    records: &ProjectRecords,
    prior: Option<(&Manifest, &ProjectRecords)>,
    mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<Manifest> {
    let mut payload = |value: &Value, original: Option<(&Value, &String)>| {
        if let Some((old, hash)) = original
            && value == old
        {
            return Ok(hash.clone());
        }
        write_payload(folder, value, &mut write_new)
    };
    let envelope = payload(&records.envelope, prior.map(|(m, r)| (&r.envelope, &m.envelope)))?;
    let mut group = |values: &BTreeMap<u64, Value>, prior: Option<PriorGroup<'_>>| -> io::Result<Vec<_>> {
        let hashes: BTreeMap<_, _> = prior.into_iter().flat_map(|(list, _)| list.iter().map(|(id, h)| (*id, h))).collect();
        values
            .iter()
            .map(|(id, v)| {
                let original = prior.and_then(|(_, records)| records.get(id)).zip(hashes.get(id).copied());
                Ok((*id, payload(v, original)?))
            })
            .collect()
    };
    let bins = group(&records.bins, prior.map(|(m, r)| (m.bins.as_slice(), &r.bins)))?;
    let items = group(&records.items, prior.map(|(m, r)| (m.items.as_slice(), &r.items)))?;
    Ok(Manifest { format: FORMAT.to_owned(), layout: 1, revision: None, envelope, bins, items })
}

pub fn pack(source: &Path, folder: &Path, mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<()> {
    let records = records_from(source, folder)?;
    fs::create_dir(folder)?;
    let root = folder.join(ROOT);
    let manifest = materialize(folder, &records, None, &mut write_new)?;
    let bytes = serde_json::to_vec(&manifest)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(invalid("manifest exceeds the folder layout's size limit"));
    }
    assemble(folder, &manifest, &root)?;
    #[cfg(unix)]
    fs::File::open(folder)?.sync_all()?;
    write_new(&root, &bytes)
}

pub fn unpack(folder: &Path, destination: &Path) -> io::Result<()> {
    let manifest = serde_json::from_slice(&read_member(&folder.join(ROOT), MANIFEST_LIMIT)?)?;
    atomic_write_new(destination, &standard_document(&assemble(folder, &manifest, destination)?)?)
}

fn outside_folder(folder: &Path, output: &Path) -> io::Result<()> {
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if parent.canonicalize()?.starts_with(folder.canonicalize()?) {
        return Err(invalid("choose an export destination outside the project folder"));
    }
    Ok(())
}

mod overlay;
mod session;
pub use session::{EditOrigin, FolderSnapshot, relocate_document};
pub mod checkpoint;

mod shared;
pub use shared::BinScope;
