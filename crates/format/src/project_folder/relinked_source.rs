//! Publish a single reviewed file source without constructing an incomplete Project.
use super::{FolderDirectory, ROOT, digest, invalid, overlay, retention, revisions, verified_bytes};
use crate::{MAX_PROJECT_BYTES, SCHEMA_VERSION};
use filmcraft_project::{BinId, MediaRef, ProjectItem};
use std::{collections::BTreeSet, fs::File, io, path::Path};

fn folder(path: &Path) -> io::Result<&Path> {
    path.parent().filter(|_| path.file_name().is_some_and(|n| n == ROOT)).ok_or_else(|| invalid("expected project.fcmod"))
}

/// Pin a reviewed working edit (or discard) against a concurrent publication.
pub fn lock_source_revision(path: &Path, expected: &str) -> io::Result<(File, u64)> {
    let guard = revisions::writer(folder(path)?)?;
    let (rev, bytes) = FolderDirectory::published_revision_size(path)?;
    if rev != expected {
        return Err(invalid("stale project revision; refresh discovery"));
    }
    Ok((guard, bytes))
}

pub struct RelinkSave {
    pub revision: String,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub maintenance_warning: Option<String>,
}

/// The caller supplies the published before-image and the in-memory edit. Copy the
/// manifest verbatim except for the one item key and normal revision bookkeeping.
pub fn save_relinked_source(path: &Path, expected: &str, owner: BinId, before: &ProjectItem, after: &ProjectItem) -> io::Result<RelinkSave> {
    let folder = folder(path)?;
    let _guard = revisions::writer(folder)?;
    let (base, prior) = revisions::read_base(folder, expected)?;
    let directory = FolderDirectory::open(path)?;
    if directory.revision != expected || directory.envelope["schema_version"] != SCHEMA_VERSION {
        return Err(invalid("unsupported or stale source revision"));
    }
    let owners: BTreeSet<_> = directory
        .bins
        .iter()
        .filter(|(_, b)| b["children"].as_array().is_some_and(|entries| entries.iter().any(|e| e["Item"] == before.id.0)))
        .map(|(id, _)| BinId(*id))
        .collect();
    if owners != [owner].into() || prior.identities.contains_key(&before.id.0) {
        return Err(invalid("source needs one owning bin"));
    }
    let mut allowed = before.clone();
    let old = allowed.as_media_mut().ok_or_else(|| invalid("expected file source"))?;
    let new = after.as_media().ok_or_else(|| invalid("expected relinked source"))?;
    if !matches!(old.media, MediaRef::File { .. }) || !matches!(new.media, MediaRef::File { .. }) || old.identity.is_none() || old.identity != new.identity {
        return Err(invalid("expected the same fingerprinted file source"));
    }
    old.media = new.media.clone();
    old.info = new.info.clone();
    old.offline = new.offline;
    if &allowed != after {
        return Err(invalid("relink changed unrelated source fields"));
    }
    let mut remaining = directory.remaining_item_bytes;
    let mut replacement = None;
    let mut old_len = 0;
    // A warm index is a lookup accelerator, not evidence that a file is still intact.
    // Hash the other immutable records without decoding them before retaining their keys.
    for (id, hash) in &prior.items {
        let bytes = verified_bytes(folder, hash, &mut remaining)?;
        if *id == before.id.0 {
            let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
            if serde_json::from_value::<ProjectItem>(raw.clone())? != *before {
                return Err(invalid("source differs from saved before-image"));
            }
            old_len = bytes.len() as u64;
            replacement = Some(serde_json::to_vec(&overlay::record(&raw, &serde_json::to_value(before)?, &serde_json::to_value(after)?, "relinked source")?)?);
        }
    }
    let bytes_read = base.len() as u64 + directory.bytes_read + directory.remaining_item_bytes - remaining;
    let data = replacement.ok_or_else(|| invalid("missing required source record"))?;
    if before == after {
        return Ok(RelinkSave { revision: expected.into(), bytes_read, bytes_written: 0, maintenance_warning: None });
    }
    if data.len() as u64 > remaining + old_len {
        return Err(invalid("project exceeds the record budget"));
    }
    let hash = digest(&data);
    let mut bytes_written = 0;
    let mut bytes_read = bytes_read;
    match crate::atomic_write_new(&folder.join(&hash), &data) {
        Ok(()) => bytes_written += data.len() as u64,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mut budget = MAX_PROJECT_BYTES;
            verified_bytes(folder, &hash, &mut budget)?;
            bytes_read += data.len() as u64;
        }
        Err(e) => return Err(e),
    }
    let mut next = prior.clone();
    next.items.iter_mut().find(|(id, _)| *id == before.id.0).ok_or_else(|| invalid("missing required source record"))?.1 = hash;
    let transfers = revisions::Transfers {
        backed_up_bins: [owner.0].into(),
        active_move_bases: prior.revision.as_ref().map(|r| r.active_move_bases.clone()).unwrap_or_default(),
        ..Default::default()
    };
    if folder.join(expected).try_exists()? {
        bytes_read += base.len() as u64;
    }
    let revision = revisions::commit(
        folder,
        &base,
        &prior,
        &mut next,
        transfers,
        |_| Ok(()),
        |p, b| {
            revisions::publish(p, b)?;
            bytes_written += b.len() as u64;
            Ok(())
        },
    )?;
    let maintenance_warning = retention::after_publication(folder, &revision, &next).err().map(|e| e.to_string());
    Ok(RelinkSave { revision, bytes_read, bytes_written, maintenance_warning })
}
