//! Native, content-addressed project-folder storage. See docs/project-files.md.
use crate::{MAX_PROJECT_BYTES, SCHEMA_VERSION, atomic_write_new};
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_media::reader::read_regular_file_bounded;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

mod editor;
mod overlay;
mod paths;
mod recovery;
mod retention;
mod revisions;
pub use retention::{RetentionPolicy, RetentionPreview, configure_retention, keep_version, preview_retention, prune_history, retention_policy};

pub use editor::checkpoint::{CheckpointStore, CheckpointWrite, load_checkpoint, remove_checkpoint_records};
pub use editor::{Departure, DepartureCut, FolderSnapshot, SaveResult, TransferSave, detached_project};
pub use recovery::{history, move_backups, rescue, restore_copy};
pub use revisions::{checkout, save, save_with};

pub const ROOT: &str = "project.fcmod";
const FORMAT: &str = "filmcraft.project-folder";
const MANIFEST_LIMIT: u64 = 16 << 20;
const RECORD_LIMIT: usize = 100_000;
const SHARED_SCHEMA_VERSION: u32 = craft_fcproj_format::SCHEMA_VERSION;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    layout: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<revisions::Revision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    copies: Vec<BinCopy>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    identities: BTreeMap<u64, CopyIdentity>,
    envelope: String,
    bins: Vec<(u64, String)>,
    items: Vec<(u64, String)>,
}

/// A saved source bin keeps its original bytes until its own removal is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinCopy {
    pub bin: u64,
    pub original: u64,
    pub copy: u64,
}

/// Recoverable sequence at departure. The containing revision orders this move.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MoveBackup {
    pub source: u64,
    pub destination: u64,
    pub sequence: u64,
    pub snapshot: String,
    pub saved_source: String,
}

/// A surviving cut initially shares the old immutable item payload. Its
/// project identity and display name are materialized on read and first edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CopyIdentity {
    stored: u64,
    name: String,
}

impl CopyIdentity {
    fn apply(&self, id: u64, data: &mut Value) -> io::Result<()> {
        if data["id"].as_u64() != Some(self.stored) || !data["kind"]["Sequence"].is_object() {
            return Err(invalid("invalid sequence copy payload"));
        }
        data["id"] = id.into();
        data["created"] = id.into();
        data["name"] = self.name.clone().into();
        Ok(())
    }
}
impl BinCopy {
    fn replace(&self, bin: &mut Value, reverse: bool) -> io::Result<()> {
        let (from, to) = if reverse { (self.copy, self.original) } else { (self.original, self.copy) };
        let entries = bin["children"].as_array_mut().ok_or_else(|| invalid("missing bin entries"))?;
        let mut matches = entries.iter_mut().filter(|e| e["Item"].as_u64() == Some(from));
        let first = matches.next().ok_or_else(|| invalid("missing moved sequence in saved bin"))?;
        if matches.next().is_some() || from == to {
            return Err(invalid("ambiguous moved sequence in saved bin"));
        }
        first["Item"] = to.into();
        Ok(())
    }
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

fn is_digest(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_hash(hash: &str) -> io::Result<()> {
    if !is_digest(hash) {
        return Err(invalid("invalid SHA-256 digest"));
    }
    Ok(())
}

fn load_payload(folder: &Path, hash: &str, remaining: &mut u64) -> io::Result<Value> {
    check_hash(hash)?;
    let bytes = read_member(&folder.join(hash), *remaining)?;
    decode_payload(hash, bytes, remaining)
}

fn decode_payload(hash: &str, bytes: Vec<u8>, remaining: &mut u64) -> io::Result<Value> {
    if digest(&bytes) != hash {
        return Err(invalid("SHA-256 mismatch"));
    }
    *remaining -= bytes.len() as u64;
    Ok(serde_json::from_slice(&bytes)?)
}

fn load_group(
    load: &mut impl FnMut(&str, &mut u64) -> io::Result<Value>,
    kind: &str,
    entries: &[(u64, String)],
    identities: Option<&BTreeMap<u64, CopyIdentity>>,
    budget: &mut u64,
) -> io::Result<BTreeMap<u64, Value>> {
    let mut group = BTreeMap::new();
    for (id, hash) in entries {
        let mut data = load(hash, budget).map_err(|e| invalid(format!("{kind} {id}: {e}")))?;
        if let Some(identity) = identities.and_then(|ids| ids.get(id)) {
            identity.apply(*id, &mut data)?;
        }
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
    if let Some(revision) = &manifest.revision {
        revision.validate()?;
    }
    if manifest.bins.len().saturating_add(manifest.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    Ok(())
}

fn read_records(folder: &Path, manifest: &Manifest) -> io::Result<ProjectRecords> {
    read_records_with(manifest, |hash, budget| load_payload(folder, hash, budget))
}

fn read_records_with(manifest: &Manifest, mut load: impl FnMut(&str, &mut u64) -> io::Result<Value>) -> io::Result<ProjectRecords> {
    check_manifest(manifest)?;
    let mut budget = MAX_PROJECT_BYTES;
    let envelope = load(&manifest.envelope, &mut budget).map_err(|e| invalid(format!("project metadata: {e}")))?;
    let mut bins = load_group(&mut load, "bin", &manifest.bins, None, &mut budget)?;
    let items = load_group(&mut load, "item", &manifest.items, Some(&manifest.identities), &mut budget)?;
    for alias in &manifest.copies {
        if !items.contains_key(&alias.copy) {
            return Err(invalid("missing independent sequence copy"));
        }
        alias.replace(bins.get_mut(&alias.bin).ok_or_else(|| invalid("missing saved source bin"))?, false)?;
    }
    Ok(ProjectRecords { envelope, bins, items })
}

fn assemble(folder: &Path, manifest: &Manifest, destination: &Path) -> io::Result<Vec<u8>> {
    encode_at(folder, read_records(folder, manifest)?, destination)
}

fn encode_at(folder: &Path, mut records: ProjectRecords, destination: &Path) -> io::Result<Vec<u8>> {
    paths::rebase(&mut records, &folder.join(ROOT), destination)?;
    let bytes = serde_json::to_vec(&join(records)?)?;
    crate::decode(&bytes).map_err(invalid)?;
    Ok(bytes)
}

fn join(mut records: ProjectRecords) -> io::Result<Value> {
    let app_schema = records.envelope.get("schema_version").and_then(Value::as_u64) == Some(u64::from(SCHEMA_VERSION));
    if app_schema {
        // The shared record API checks its own schema. Only the envelope stamp differs.
        records.envelope["schema_version"] = SHARED_SCHEMA_VERSION.into();
    }
    let mut document = records.into_document().map_err(invalid)?;
    if app_schema {
        document["schema_version"] = SCHEMA_VERSION.into();
    }
    Ok(document)
}

fn write_payload(folder: &Path, value: Value, previous: Option<&str>, write_new: &mut impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<String> {
    let bytes = serde_json::to_vec(&value)?;
    let hash = digest(&bytes);
    if let Some(previous) = previous
        && previous != hash
    {
        // A native editor record can have a different JSON key order from a split
        // checkout. Keep the existing record when the JSON values are equal.
        let mut budget = MAX_PROJECT_BYTES;
        if load_payload(folder, previous, &mut budget)? == value {
            return Ok(previous.to_owned());
        }
    }
    let path = folder.join(&hash);
    match write_new(&path, &bytes) {
        Ok(()) => (),
        // Equal content may share a payload. assemble verifies every referenced byte before publication.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    Ok(hash)
}

/// Split a standard project for verified publication into an existing folder.
pub fn records_from(source: &Path, folder: &Path) -> io::Result<ProjectRecords> {
    let mut records = {
        let bytes = read_regular_file_bounded(source, MAX_PROJECT_BYTES)?;
        let schema = crate::decode(&bytes).map_err(invalid)?.schema_version;
        if schema > SHARED_SCHEMA_VERSION {
            // Preserve the original JSON, including Script, RAW, and extension data.
            let mut document: Value = serde_json::from_slice(&bytes)?;
            document["schema_version"] = SHARED_SCHEMA_VERSION.into();
            ProjectRecords::split(&serde_json::to_vec(&document)?)
        } else {
            ProjectRecords::split(&bytes)
        }
        .map_err(invalid)?
    };
    records.envelope["schema_version"] = SCHEMA_VERSION.into();
    if records.bins.len().saturating_add(records.items.len()) > RECORD_LIMIT {
        return Err(invalid("too many project records"));
    }
    let root = folder.join(ROOT);
    paths::rebase(&mut records, source, &root)?;
    Ok(records)
}

fn materialize(
    folder: &Path,
    records: ProjectRecords,
    previous: Option<&Manifest>,
    mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<Manifest> {
    let envelope = write_payload(folder, records.envelope, previous.map(|m| m.envelope.as_str()), &mut write_new)?;
    let mut group = |values: BTreeMap<u64, Value>, prior: Option<&[(u64, String)]>| -> io::Result<Vec<_>> {
        let prior: BTreeMap<_, _> = prior.unwrap_or_default().iter().map(|(id, hash)| (*id, hash.as_str())).collect();
        values.into_iter().map(|(id, value)| Ok((id, write_payload(folder, value, prior.get(&id).copied(), &mut write_new)?))).collect()
    };
    Ok(Manifest {
        format: FORMAT.to_owned(),
        layout: 1,
        copies: Vec::new(),
        identities: BTreeMap::new(),
        revision: None,
        envelope,
        bins: group(records.bins, previous.map(|m| m.bins.as_slice()))?,
        items: group(records.items, previous.map(|m| m.items.as_slice()))?,
    })
}

pub fn pack(source: &Path, folder: &Path, mut write_new: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<()> {
    let records = records_from(source, folder)?;
    fs::create_dir(folder)?; // Exclusively reserve a fresh destination. Never adopt an existing directory.
    let root = folder.join(ROOT);
    let manifest = materialize(folder, records, None, &mut write_new)?;
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
    atomic_write_new(destination, &assemble(folder, &manifest, destination)?)
}
