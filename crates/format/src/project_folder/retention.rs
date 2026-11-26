//! Account for the immutable files in one published history under a proposed keep policy.
use super::{MANIFEST_LIMIT, ROOT, check_hash, checkpoint, digest, invalid, pins, read_member, recovery};
use crate::MAX_PROJECT_BYTES;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize)]
pub struct Holds {
    pub recent: bool,
    pub milestone: bool,
    pub supplied_reader: bool,
    pub supplied_draft: bool,
}

impl Holds {
    fn include(&mut self, other: &Self) {
        self.recent |= other.recent;
        self.milestone |= other.milestone;
        self.supplied_reader |= other.supplied_reader;
        self.supplied_draft |= other.supplied_draft;
    }

    fn any(&self) -> bool {
        self.recent || self.milestone || self.supplied_reader || self.supplied_draft
    }
}

#[derive(Serialize)]
pub struct Save {
    pub revision: String,
    pub number: u64,
    pub label: Option<String>,
    pub holds: Holds,
}

#[derive(Default, Serialize)]
pub struct Member {
    pub bytes: u64,
    pub manifest_chain: bool,
    pub payload_revision_count: usize,
    pub holds: Holds,
}

#[derive(Serialize)]
pub struct Preview {
    pub head: String,
    pub keep_latest: NonZeroUsize,
    pub saves: Vec<Save>,
    pub members: BTreeMap<String, Member>,
    pub retained_policy_bytes: u64,
    pub older_only_payload_bytes: u64,
    pub unreferenced_digest_files: usize,
    pub unreferenced_digest_bytes: u64,
    pub confirmed_reclaimable_bytes: u64,
    pub protection_scope: &'static str,
}

/// Return matching journal directories without taking ownership of running or recoverable sessions.
/// A missing recovery store is distinct from a store that was successfully inspected and is empty.
pub fn draft_sessions(root: &Path, folder: &Path) -> io::Result<Option<Vec<PathBuf>>> {
    match root.symlink_metadata() {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(invalid(format!("expected a recovery directory at {}", root.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let manifest = folder.canonicalize()?.join(ROOT);
    let mut drafts = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        match checkpoint::Checkpoint::read(&dir) {
            Ok(c) if c.source.manifest == manifest => drafts.push(dir),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(invalid(format!("cannot inspect checkpoint in {}: {e}", dir.display()))),
        }
    }
    Ok(Some(drafts))
}

pub fn preview(folder: &Path, keep_latest: NonZeroUsize, readers: &[String], drafts: &[PathBuf]) -> io::Result<Preview> {
    let folder = folder.canonicalize()?;
    let mut readers: BTreeSet<_> = readers.iter().map(String::as_str).collect();
    for reader in &readers {
        check_hash(reader)?;
    }
    let mut draft_bases = BTreeSet::new();
    for dir in drafts {
        let source = checkpoint::Checkpoint::read(dir)?.source;
        if !source.matches(&checkpoint::Source::capture(&folder, &source.head)?) {
            return Err(invalid(format!("draft at {} belongs to a different or unverifiable project folder", dir.display())));
        }
        draft_bases.insert(source.head);
    }
    let pin_snapshot = pins::read(&folder)?;
    let mut labels: BTreeMap<_, _> = pin_snapshot.iter().map(|p| (p.revision.as_str(), &p.label)).collect();
    let root = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
    let head = digest(&root);
    let mut saves = Vec::new();
    let mut members = BTreeMap::<String, Member>::new();
    recovery::walk_from(&folder, root, |revision, manifest, bytes| {
        let path = if saves.is_empty() { ROOT } else { revision };
        let member = members.entry(path.to_owned()).or_default();
        member.manifest_chain = true;
        member.bytes = bytes.len() as u64;
        let label = labels.remove(revision).cloned();
        let holds = Holds {
            recent: saves.len() < keep_latest.get(),
            milestone: label.is_some(),
            supplied_reader: readers.remove(revision),
            supplied_draft: draft_bases.remove(revision),
        };
        let mut budget = MAX_PROJECT_BYTES;
        let mut seen = BTreeSet::new();
        for hash in std::iter::once(&manifest.envelope).chain(manifest.bins.iter().chain(&manifest.items).map(|(_, hash)| hash)) {
            check_hash(hash)?;
            let record = match members.entry(hash.clone()) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => {
                    let bytes = read_member(&folder.join(hash), budget).map_err(|error| invalid(format!("revision {revision}, payload {hash}: {error}")))?;
                    if digest(&bytes) != *hash {
                        return Err(invalid(format!("revision {revision}, damaged payload {hash}")));
                    }
                    e.insert(Member { bytes: bytes.len() as u64, ..Member::default() })
                }
            };
            budget = budget.checked_sub(record.bytes).ok_or_else(|| invalid("project exceeds record size budget"))?;
            if seen.insert(hash) {
                record.payload_revision_count += 1;
                record.holds.include(&holds);
            }
        }
        saves.push(Save { revision: revision.to_owned(), number: manifest.revision.map_or(0, |r| r.number), label, holds });
        Ok(false)
    })?;
    if let Some(missing) = labels.keys().copied().chain(readers).chain(draft_bases.iter().map(String::as_str)).next() {
        return Err(invalid(format!("protected revision {missing} is outside the published history")));
    }
    let mut retained_policy_bytes = 0;
    let mut older_only_payload_bytes = 0;
    for m in members.values() {
        if m.manifest_chain || m.holds.any() {
            retained_policy_bytes += m.bytes;
        } else {
            older_only_payload_bytes += m.bytes;
        }
    }
    let (unreferenced_digest_files, unreferenced_digest_bytes) = loose_files(&folder, &members)?;
    if digest(&read_member(&folder.join(ROOT), MANIFEST_LIMIT)?) != head || pins::read(&folder)? != pin_snapshot {
        return Err(io::Error::other("published head or pins changed during preview; retry"));
    }
    Ok(Preview {
        head,
        keep_latest,
        saves,
        members,
        retained_policy_bytes,
        older_only_payload_bytes,
        unreferenced_digest_files,
        unreferenced_digest_bytes,
        confirmed_reclaimable_bytes: 0,
        protection_scope: "Published history and pins, plus supplied reader revisions and draft bases. Other sessions may exist.",
    })
}

fn loose_files(folder: &Path, members: &BTreeMap<String, Member>) -> io::Result<(usize, u64)> {
    // Do not follow links or open loose files: a digest-shaped name can be user data or an in-flight save.
    let mut unreferenced_digest_bytes = 0u64;
    let mut unreferenced_digest_files = 0;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(name) = name.to_str()
            && check_hash(name).is_ok()
            && !members.contains_key(name)
        {
            let meta = entry.path().symlink_metadata()?;
            if meta.is_file() {
                unreferenced_digest_files += 1;
                unreferenced_digest_bytes = unreferenced_digest_bytes.checked_add(meta.len()).ok_or_else(|| invalid("loose file sizes overflow"))?;
            }
        }
    }
    Ok((unreferenced_digest_files, unreferenced_digest_bytes))
}
