//! Publish an edit to one verified resident item without serializing a partial project.
use super::{FolderDirectory, ROOT, digest, invalid, overlay, retention, revisions, verified_bytes};
use crate::{MAX_PROJECT_BYTES, SCHEMA_VERSION};
use filmcraft_project::{BinId, MediaRef, Project, ProjectItem, ProjectView};
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
    save_resident_records(path, expected, Some((owner, before, after)), None, None, None)
}

/// Publish the active timeline record and/or the current global view in one revision.
pub fn save_trimmed_sequence(
    path: &Path,
    expected: &str,
    edit: Option<(BinId, &ProjectItem, &ProjectItem)>,
    view: Option<&ProjectView>,
    next_id: Option<u64>,
) -> io::Result<RelinkSave> {
    if let Some((_, before, after)) = edit {
        let mut allowed = before.clone();
        allowed.kind = after.kind.clone();
        if allowed != *after || before.as_sequence().is_none() {
            return Err(invalid("expected the same sequence"));
        }
        after.as_sequence().ok_or_else(|| invalid("missing sequence"))?.check_bounds().map_err(invalid)?;
    }
    save_resident_records(path, expected, edit, view, next_id, None)
}

/// Splits can consume IDs even for segments removed by the extract. Preserve the
/// actual counter together with the cut in both the saved and recovery revisions.
pub(super) fn set_trim_counter(envelope: &mut serde_json::Value, next: Option<u64>, edited: Option<&ProjectItem>) -> io::Result<bool> {
    let Some(next) = next else { return Ok(false) };
    let item = edited.filter(|item| item.as_sequence().is_some()).ok_or_else(|| invalid("missing trimmed sequence"))?;
    if next > Project::MAX_NEXT_ID || next <= item.max_id() {
        return Err(invalid("invalid ID counter for trimmed sequence"));
    }
    let field = &mut envelope["project"]["next_id"];
    let changed = field.as_u64() != Some(next);
    *field = next.into();
    Ok(changed)
}

fn stage(folder: &Path, hash: &str, data: &[u8], written: &mut u64, read: &mut u64) -> io::Result<()> {
    match super::publication::stage_record(&folder.join(hash), data) {
        Ok(()) => *written += data.len() as u64,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mut budget = MAX_PROJECT_BYTES;
            verified_bytes(folder, hash, &mut budget)?;
            *read += data.len() as u64;
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// The editor has only changed folder names or the placement of existing bins.
pub fn save_directory(path: &Path, expected: &str, project: &Project, view: Option<&ProjectView>) -> io::Result<RelinkSave> {
    project.validate_directory().map_err(invalid)?;
    save_resident_records(path, expected, None, view, None, Some(project))
}

pub(crate) fn apply_directory(envelope: &mut serde_json::Value, project: &Project) -> io::Result<()> {
    let metadata = envelope["project"].as_object_mut().ok_or_else(|| invalid("missing project metadata"))?;
    if let Some(dir) = &project.modular_directory {
        metadata.insert("modular_directory".into(), serde_json::to_value(dir)?);
    } else {
        metadata.remove("modular_directory");
    }
    if project.next_id > Project::MAX_NEXT_ID {
        return Err(invalid("project ID limit reached"));
    }
    metadata.insert("next_id".into(), project.next_id.into());
    Ok(())
}

fn save_resident_records(
    path: &Path,
    expected: &str,
    edit: Option<(BinId, &ProjectItem, &ProjectItem)>,
    view: Option<&ProjectView>,
    next_id: Option<u64>,
    organization: Option<&Project>,
) -> io::Result<RelinkSave> {
    let folder = folder(path)?;
    let _guard = revisions::writer(folder)?;
    let (base, prior) = revisions::read_base(folder, expected)?;
    let directory = FolderDirectory::open(path)?;
    if directory.revision != expected || directory.envelope["schema_version"] != SCHEMA_VERSION {
        return Err(invalid("unsupported or stale source revision"));
    }
    if let Some((owner, before, _)) = edit {
        let owners: BTreeSet<_> = directory
            .bins
            .iter()
            .filter(|(_, b)| b["children"].as_array().is_some_and(|entries| entries.iter().any(|e| e["Item"] == before.id.0)))
            .map(|(id, _)| BinId(*id))
            .collect();
        if owners != [owner].into() || !prior.items.iter().any(|(id, _)| *id == before.id.0) || prior.identities.contains_key(&before.id.0) {
            return Err(invalid("edited item needs one owning bin"));
        }
    }
    let mut remaining = directory.remaining_item_bytes;
    let mut next = prior.clone();
    let mut pending = Vec::new();
    let mut delta = 0i64;
    let mut bytes_written = 0;
    let mut bytes_read = base.len() as u64 + directory.bytes_read;
    let mut backed_up_bins = BTreeSet::new();
    for (id, hash) in &mut next.items {
        let bytes = verified_bytes(folder, hash, &mut remaining)?;
        bytes_read += bytes.len() as u64;
        if let Some((owner, before, after)) = edit.filter(|(_, before, _)| before.id.0 == *id) {
            let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
            if serde_json::from_value::<ProjectItem>(raw.clone())? != *before {
                return Err(invalid("item differs from saved before-image"));
            }
            if before != after {
                let data = serde_json::to_vec(&overlay::record(&raw, &serde_json::to_value(before)?, &serde_json::to_value(after)?, "edited item")?)?;
                delta += data.len() as i64 - bytes.len() as i64;
                if serde_json::from_slice::<ProjectItem>(&data)? != *after {
                    return Err(invalid("published item differs from edit"));
                }
                *hash = digest(&data);
                pending.push((hash.clone(), data));
                backed_up_bins.insert(owner.0);
            }
        }
    }
    let mut envelope = directory.envelope;
    let counter_changed = set_trim_counter(&mut envelope, next_id, edit.map(|(_, _, after)| after))?;
    if let Some(project) = organization {
        apply_directory(&mut envelope, project)?;
        next.layout = 3;
    }
    if view.is_some() || counter_changed || organization.is_some() {
        let mut budget = MAX_PROJECT_BYTES;
        let old = verified_bytes(folder, &prior.envelope, &mut budget)?;
        bytes_read += old.len() as u64;
        if let Some(view) = view {
            let saved: ProjectView = serde_json::from_value(envelope["view"].clone())?;
            envelope["view"] = overlay::record(&envelope["view"], &serde_json::to_value(saved)?, &serde_json::to_value(view)?, "project view")?;
        }
        let data = serde_json::to_vec(&envelope)?;
        delta += data.len() as i64 - old.len() as i64;
        next.envelope = digest(&data);
        pending.push((next.envelope.clone(), data));
    }
    if delta > remaining as i64 {
        return Err(invalid("project exceeds the record budget"));
    }
    if next.envelope == prior.envelope && next.items == prior.items {
        return Ok(RelinkSave { revision: expected.into(), bytes_read, bytes_written, maintenance_warning: None });
    }
    for (hash, bytes) in pending {
        stage(folder, &hash, &bytes, &mut bytes_written, &mut bytes_read)?;
    }
    let transfers = revisions::Transfers {
        backed_up_bins,
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
