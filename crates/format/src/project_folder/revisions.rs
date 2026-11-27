//! Local writer serialization and optimistic revision checks. The manifest rename is the
//! commit point. Older manifests and payloads stay available by digest.
use super::{MANIFEST_LIMIT, Manifest, ROOT, assemble, digest, invalid, is_digest, materialize, read_member, records_from};
use crate::atomic_write_new;
use craft_fcproj_format::records::ProjectRecords;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    pub(super) number: u64,
    pub(super) parent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) moves: Vec<super::MoveBackup>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    arrivals: BTreeMap<u64, u64>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) active_move_bases: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) backed_up_bins: BTreeSet<u64>,
    #[serde(default)]
    pub(super) total_bin_backups: u64,
    bin_edits: BTreeMap<u64, Edit>,
    item_edits: BTreeMap<u64, Edit>,
}

#[derive(Default)]
pub(super) struct Transfers {
    pub backed_up_bins: BTreeSet<u64>,
    pub backups: Vec<super::MoveBackup>,
    pub arrivals: BTreeMap<u64, u64>,
    pub active_move_bases: BTreeSet<String>,
}

impl Revision {
    pub(super) fn saved_at(&self) -> Option<String> {
        self.bin_edits
            .values()
            .chain(self.item_edits.values())
            .find(|e| e.revision == self.number)
            .and_then(|e| chrono::DateTime::from_timestamp_millis(e.utc_ms))
            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        if self.number == 0
            || !is_digest(&self.parent)
            || self.active_move_bases.iter().any(|hash| !is_digest(hash))
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
    let file = lock_file(&folder.join(".writer.lock"), true)?;
    match try_writer(file) {
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            // A foreground writer may wait for the maintenance commit window.
            // Two ordinary editors retain the existing optimistic-lock behaviour.
            let gate = match lock_file(&folder.join(MAINTENANCE_GATE), false) {
                Ok(gate) => gate,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(e),
                Err(err) => return Err(err),
            };
            gate.lock_shared()?;
            try_writer(lock_file(&folder.join(".writer.lock"), true)?)
        }
        other => other,
    }
}

const MAINTENANCE_GATE: &str = ".retention-commit.lock";

/// Hold the admission gate until the short catalog or unlink transaction ends.
/// File verification and history traversal must happen outside these locks.
pub(super) fn maintenance_writer(folder: &Path) -> io::Result<(File, File)> {
    let gate = try_writer(lock_file(&folder.join(MAINTENANCE_GATE), true)?)?;
    let writer = try_writer(lock_file(&folder.join(".writer.lock"), true)?)?;
    // Drop the writer lock before reopening the foreground admission gate.
    Ok((writer, gate))
}

/// Elect one planner across editor sessions without occupying the project writer.
pub(super) fn retention_worker(folder: &Path) -> io::Result<File> {
    let lock = lock_file(&folder.join(".retention-worker.lock"), true)?;
    lock.lock()?;
    Ok(lock)
}

fn lock_file(path: &Path, create: bool) -> io::Result<File> {
    match path.symlink_metadata() {
        Ok(meta) if !meta.is_file() => return Err(invalid("project lock must be a regular file")),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    OpenOptions::new().create(create).truncate(false).read(true).write(true).open(path)
}

fn try_writer(file: File) -> io::Result<File> {
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
    let bins = records.bins.clone();
    let mut budget = crate::MAX_PROJECT_BYTES;
    let old_meta = super::load_payload(folder, &previous.envelope, &mut budget)?;
    let attached = changed_attachments(&old_meta, &records.envelope);
    let mut next = materialize(folder, records, Some(&previous), |p, b| {
        // All reused bytes are verified by assemble before publishing the candidate.
        if p.try_exists()? { Ok(()) } else { write(p, b) }
    })?;
    let transfers = Transfers { backed_up_bins: changed_bins(&previous, &next, &bins, attached), ..Default::default() };
    let saved =
        commit(folder, &previous_bytes, &previous, &mut next, transfers, |candidate| assemble(folder, candidate, &folder.join(ROOT)).map(|_| ()), write)?;
    if saved != expected
        && let Err(error) = super::retention::after_publication(folder, &saved, &next)
    {
        log::warn!("project saved; recovery maintenance: {error}");
    }
    Ok(saved)
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
        total_bin_backups: prior.map_or(0, |r| r.total_bin_backups).saturating_add(transfers.backed_up_bins.len() as u64),
        backed_up_bins: transfers.backed_up_bins,
        active_move_bases: transfers.active_move_bases,
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

// Bin membership and the payload digests already exist in memory during a save.
pub(super) fn changed_bins(previous: &Manifest, next: &Manifest, bins: &BTreeMap<u64, serde_json::Value>, mut items: BTreeSet<u64>) -> BTreeSet<u64> {
    fn changed(old: &[(u64, String)], new: &[(u64, String)]) -> BTreeSet<u64> {
        if old == new {
            return BTreeSet::new();
        }
        let old: BTreeMap<_, _> = old.iter().map(|(id, h)| (*id, h)).collect();
        let new: BTreeMap<_, _> = new.iter().map(|(id, h)| (*id, h)).collect();
        old.keys().chain(new.keys()).filter(|id| old.get(id) != new.get(id)).copied().collect()
    }
    let mut saved = changed(&previous.bins, &next.bins);
    items.extend(changed(&previous.items, &next.items));
    if !items.is_empty() {
        let aliases: BTreeMap<_, _> = next.copies.iter().map(|a| ((a.bin, a.original), a.copy)).collect();
        for (&bin, record) in bins {
            if record["children"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["Item"].as_u64())
                .any(|id| items.contains(aliases.get(&(bin, id)).unwrap_or(&id)))
            {
                saved.insert(bin);
            }
        }
    }
    saved
}

pub(super) fn changed_attachments(old: &serde_json::Value, new: &serde_json::Value) -> BTreeSet<u64> {
    let mut changed = BTreeSet::new();
    for key in super::recovery::SIDECARS {
        let old = &old["project"][key];
        let new = &new["project"][key];
        if old != new {
            for id in [old, new].into_iter().filter_map(|v| v.as_object()).flat_map(|m| m.keys()) {
                if old[id] != new[id]
                    && let Ok(id) = id.parse()
                {
                    changed.insert(id);
                }
            }
        }
    }
    changed
}
