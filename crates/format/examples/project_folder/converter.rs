//! Developer-only project-folder conversion. See docs/project-files.md.
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_format::{MAX_PROJECT_BYTES, atomic_write_new};
use filmcraft_media::reader::read_regular_file_bounded;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[path = "bins.rs"]
pub(super) mod bins;
#[path = "paths.rs"]
mod paths;
#[path = "recovery.rs"]
pub(super) mod recovery;
#[path = "revisions.rs"]
pub(super) mod revisions;

pub(super) const ROOT: &str = "project.fcmod";
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
    filmcraft_format::decode(&bytes).map_err(invalid)?;
    Ok(bytes)
}

fn encode_at(folder: &Path, records: ProjectRecords, destination: &Path) -> io::Result<Vec<u8>> {
    checked_bytes(&document_at(folder, records, destination)?)
}

fn write_payload(folder: &Path, value: Value, write_new: &mut impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<String> {
    let bytes = serde_json::to_vec(&value)?;
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
        filmcraft_format::decode(&bytes).map_err(invalid)?;
        ProjectRecords::split(&bytes).map_err(invalid)?
    };
    if records.bins.len().saturating_add(records.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    let root = folder.join(ROOT);
    paths::rebase(&mut records, source, &root)?;
    Ok(records)
}

fn materialize(folder: &Path, records: ProjectRecords, mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<Manifest> {
    let envelope = write_payload(folder, records.envelope, &mut write_new)?;
    let mut group = |values: BTreeMap<u64, Value>| -> io::Result<Vec<_>> {
        values.into_iter().map(|(id, value)| Ok((id, write_payload(folder, value, &mut write_new)?))).collect()
    };
    Ok(Manifest { format: FORMAT.to_owned(), layout: 1, revision: None, envelope, bins: group(records.bins)?, items: group(records.items)? })
}

pub(super) fn pack(source: &Path, folder: &Path, mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<()> {
    let records = records_from(source, folder)?;
    fs::create_dir(folder)?;
    let root = folder.join(ROOT);
    let manifest = materialize(folder, records, &mut write_new)?;
    let bytes = serde_json::to_vec(&manifest)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(invalid("manifest exceeds the folder layout's size limit"));
    }
    assemble(folder, &manifest, &root)?;
    #[cfg(unix)]
    fs::File::open(folder)?.sync_all()?;
    write_new(&root, &bytes)
}

pub(super) fn unpack(folder: &Path, destination: &Path) -> io::Result<()> {
    let manifest = serde_json::from_slice(&read_member(&folder.join(ROOT), MANIFEST_LIMIT)?)?;
    atomic_write_new(destination, &assemble(folder, &manifest, destination)?)
}
