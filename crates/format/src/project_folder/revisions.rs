//! Local writer serialization and optimistic revision checks. The manifest rename is the
//! commit point. Older manifests and payloads stay available by digest.
use super::{MANIFEST_LIMIT, Manifest, ROOT, assemble, digest, invalid, is_digest, materialize, read_member, records_from};
use crate::atomic_write_new;
use craft_fcproj_format::records::ProjectRecords;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::BTreeMap, io, path::Path};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    pub(super) number: u64,
    pub(super) parent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) moves: Vec<super::MoveBackup>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    arrivals: BTreeMap<u64, u64>,
    bin_edits: BTreeMap<u64, Edit>,
    item_edits: BTreeMap<u64, Edit>,
}

#[derive(Default)]
pub(super) struct Transfers {
    pub backups: Vec<super::MoveBackup>,
    pub arrivals: BTreeMap<u64, u64>,
}

impl Revision {
    pub(super) fn validate(&self) -> io::Result<()> {
        if self.number == 0
            || !is_digest(&self.parent)
            || self.moves.iter().any(|m| m.source == m.destination || !is_digest(&m.snapshot) || !is_digest(&m.saved_source))
        {
            return Err(invalid("invalid project revision metadata"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Edit {
    utc_ms: i64,
    revision: u64,
}

fn edits(old: &[(u64, String)], new: &[(u64, String)], previous: Option<&BTreeMap<u64, Edit>>, edit: Edit) -> BTreeMap<u64, Edit> {
    let old: BTreeMap<_, _> = old.iter().map(|(id, hash)| (*id, hash)).collect();
    new.iter()
        .filter_map(|(id, hash)| {
            let stamp = if old.get(id).copied() == Some(hash) { previous.and_then(|m| m.get(id)).copied() } else { Some(edit) };
            stamp.map(|s| (*id, s))
        })
        .collect()
}

pub(super) fn writer(folder: &Path) -> io::Result<File> {
    let path = folder.join(".writer.lock");
    match path.symlink_metadata() {
        Ok(meta) if !meta.is_file() => return Err(invalid("project lock must be a regular file")),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(io::Error::new(io::ErrorKind::WouldBlock, "another writer holds the project lock")),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    Ok(file)
}

pub fn checkout(folder: &Path, destination: &Path) -> io::Result<String> {
    let bytes = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
    let manifest = serde_json::from_slice(&bytes)?;
    atomic_write_new(destination, &assemble(folder, &manifest, destination)?)?;
    Ok(digest(&bytes))
}

pub(super) fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.file_name().is_some_and(|name| name == ROOT) { crate::atomic_write(path, bytes) } else { atomic_write_new(path, bytes) }
}

pub fn save(folder: &Path, expected: &str, source: &Path) -> io::Result<String> {
    save_with(folder, expected, records_from(source, folder)?, publish)
}

/// Publish a complete record set with a caller-supplied atomic writer.
pub fn save_with(folder: &Path, expected: &str, records: ProjectRecords, mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>) -> io::Result<String> {
    let _guard = writer(folder)?;
    let (previous_bytes, previous) = read_base(folder, expected)?;
    assemble(folder, &previous, &folder.join(ROOT))?;
    let mut next = materialize(folder, records, Some(&previous), |p, b| {
        // All reused bytes are verified by assemble before publishing the candidate.
        if p.try_exists()? { Ok(()) } else { write(p, b) }
    })?;
    commit(folder, &previous_bytes, &previous, &mut next, Transfers::default(), |candidate| assemble(folder, candidate, &folder.join(ROOT)).map(|_| ()), write)
}

pub(super) fn read_base(folder: &Path, expected: &str) -> io::Result<(Vec<u8>, Manifest)> {
    let bytes = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
    let current = digest(&bytes);
    if expected != current {
        return Err(io::Error::other(format!("stale project revision; current revision is {current}")));
    }
    let manifest = serde_json::from_slice(&bytes)?;
    Ok((bytes, manifest))
}

pub(super) fn commit(
    folder: &Path,
    previous_bytes: &[u8],
    previous: &Manifest,
    next: &mut Manifest,
    transfers: Transfers,
    verify: impl FnOnce(&Manifest) -> io::Result<()>,
    mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<String> {
    let base = digest(previous_bytes);
    if previous.envelope == next.envelope
        && previous.items == next.items
        && previous.bins == next.bins
        && previous.identities == next.identities
        && previous.copies == next.copies
        && transfers.backups.is_empty()
        && transfers.arrivals.is_empty()
    {
        return Ok(base.to_owned());
    }
    let prior = previous.revision.as_ref();
    let number = prior.map_or(0, |r| r.number).checked_add(1).ok_or_else(|| invalid("revision limit reached"))?;
    let utc_ms = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).map_err(invalid)?.as_millis()).map_err(invalid)?;
    let edit = Edit { revision: number, utc_ms };
    next.layout = 2;
    next.revision = Some(Revision {
        number,
        parent: base.to_owned(),
        moves: transfers.backups,
        arrivals: transfers.arrivals,
        bin_edits: edits(&previous.bins, &next.bins, prior.map(|r| &r.bin_edits), edit),
        item_edits: edits(&previous.items, &next.items, prior.map(|r| &r.item_edits), edit),
    });
    let bytes = serde_json::to_vec(next)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(invalid("revision manifest exceeds size limit"));
    }
    verify(next)?;
    let archive = folder.join(&base);
    if archive.try_exists()? {
        if read_member(&archive, MANIFEST_LIMIT)? != previous_bytes {
            return Err(invalid("damaged archived manifest"));
        }
    } else {
        write(&archive, previous_bytes)?;
    }
    #[cfg(unix)]
    File::open(folder)?.sync_all()?;
    write(&folder.join(ROOT), &bytes)?;
    Ok(digest(&bytes))
}
