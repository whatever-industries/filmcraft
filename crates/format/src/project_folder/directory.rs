//! Read the published bin directory and resolve individual immutable item records
//! against that same manifest, including identity overlays for surviving sequence copies.
use super::{
    CopyIdentity, MANIFEST_LIMIT, Manifest, ROOT, check_hash, check_manifest, digest, invalid, load_group, load_payload, read_member, resolve_bin_copies,
    verified_bytes,
};
use crate::MAX_PROJECT_BYTES;
use filmcraft_project::{ItemId, ProjectItem};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

pub struct FolderDirectory {
    folder: PathBuf,
    total_bin_backups: u64,
    /// Digest of the manifest bytes captured at open.
    pub revision: String,
    pub envelope: Value,
    /// Saved bin records with move copies resolved to their effective project item IDs.
    pub bins: BTreeMap<u64, Value>,
    pub bytes_read: u64,
    /// The manifest has a separate size limit. Only the envelope and bins consume the shared payload budget at open.
    pub remaining_item_bytes: u64,
    items: BTreeMap<ItemId, Record>,
}

struct Record {
    hash: String,
    identity: Option<CopyIdentity>,
    copy_key: Option<String>,
}

impl FolderDirectory {
    /// Read only the current manifest to check whether a captured directory is still current.
    pub fn published_revision(path: &Path) -> io::Result<String> {
        Self::published_revision_size(path).map(|(revision, _)| revision)
    }

    /// Digest and number of bytes read by a manifest freshness check.
    pub fn published_revision_size(path: &Path) -> io::Result<(String, u64)> {
        let bytes = read_member(path, MANIFEST_LIMIT)?;
        Ok((digest(&bytes), bytes.len() as u64))
    }

    /// A desktop open must resume pending bin backup maintenance in the full editor.
    pub fn retention_due(&self) -> io::Result<bool> {
        Ok(super::retention::Catalog::read(&self.folder)?.due(self.total_bin_backups))
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        if path.file_name().is_none_or(|name| name != ROOT) {
            return Err(invalid("expected project.fcmod"));
        }
        let folder = path.parent().ok_or_else(|| invalid("missing project directory"))?.to_owned();
        let bytes = read_member(path, MANIFEST_LIMIT)?;
        Self::from_manifest(folder, serde_json::from_slice(&bytes)?, digest(&bytes), bytes.len() as u64)
    }

    /// Inspect one published or retained revision under the same retention policy as historical recovery.
    pub fn retained(path: &Path, revision: &str) -> io::Result<Self> {
        let folder = path.parent().filter(|_| path.file_name().is_some_and(|n| n == ROOT)).ok_or_else(|| invalid("expected project.fcmod"))?;
        let manifest = super::recovery::at(folder, revision)?;
        Self::from_manifest(folder.to_owned(), manifest, revision.to_owned(), 0)
    }

    fn from_manifest(folder: PathBuf, mut manifest: Manifest, revision: String, manifest_bytes: u64) -> io::Result<Self> {
        check_manifest(&manifest)?;
        let mut remaining = MAX_PROJECT_BYTES;
        let envelope = load_payload(&folder, &manifest.envelope, &mut remaining)?;
        let mut bins = load_group(&mut |hash, budget| load_payload(&folder, hash, budget), "bin", &manifest.bins, None, &mut remaining)?;
        let mut items = BTreeMap::new();
        for (id, hash) in manifest.items {
            check_hash(&hash)?;
            let identity = manifest.identities.remove(&id);
            let copy_key = identity.as_ref().map(|copy| serde_json::to_vec(&(id, &hash, copy)).map(|v| digest(&v))).transpose()?;
            if items.insert(ItemId(id), Record { hash, identity, copy_key }).is_some() {
                return Err(invalid("duplicate directory item"));
            }
        }
        if !manifest.identities.is_empty() {
            return Err(invalid("copy identity has no item"));
        }
        resolve_bin_copies(&manifest.copies, &mut bins, |id| items.contains_key(&ItemId(id)))?;
        Ok(Self {
            revision,
            bytes_read: manifest_bytes + (MAX_PROJECT_BYTES - remaining),
            remaining_item_bytes: remaining,
            total_bin_backups: manifest.revision.as_ref().map_or(0, |r| r.total_bin_backups),
            folder,
            envelope,
            bins,
            items,
        })
    }

    /// A cache key changes if the stored payload or its effective copy identity changes.
    pub fn item_keys(&self) -> impl Iterator<Item = (ItemId, &str)> {
        self.items.iter().map(|(&id, rec)| (id, rec.copy_key.as_deref().unwrap_or(&rec.hash)))
    }

    pub fn is_sequence_copy(&self, id: ItemId) -> bool {
        self.items.get(&id).is_some_and(|r| r.identity.is_some())
    }

    /// Charge verified payload bytes to the caller's remaining traversal budget.
    pub fn read_item(&self, id: ItemId, remaining: &mut u64) -> io::Result<ProjectItem> {
        let rec = self.items.get(&id).ok_or_else(|| invalid(format!("missing item {}", id.0)))?;
        let bytes = verified_bytes(&self.folder, &rec.hash, remaining)?;
        self.decode_item(id, &bytes)
    }

    pub(super) fn decode_item(&self, id: ItemId, bytes: &[u8]) -> io::Result<ProjectItem> {
        let rec = self.items.get(&id).ok_or_else(|| invalid("missing item"))?;
        let item: ProjectItem = if let Some(copy) = &rec.identity {
            let mut raw = serde_json::from_slice(bytes)?;
            copy.apply(id.0, &mut raw)?;
            serde_json::from_value(raw)?
        } else {
            serde_json::from_slice(bytes)?
        };
        if item.id != id {
            return Err(invalid("mismatched item identity"));
        }
        if let Some(cut) = item.as_sequence() {
            cut.check_bounds().map_err(invalid)?;
        }
        Ok(item)
    }
}
