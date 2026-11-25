//! A manual editing snapshot. The OS lock stays attached to the open document;
//! immutable records still allow a consistent reader while another writer publishes.
use super::{MANIFEST_LIMIT, ProjectRecords, ROOT, assemble, digest, encode_at, invalid, overlay, read_member, revisions};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

pub struct FolderSnapshot {
    folder: PathBuf,
    writer: Option<File>,
    revision: String,
    raw: ProjectRecords,
}

impl FolderSnapshot {
    pub fn open(manifest: &Path) -> io::Result<(Self, Vec<u8>)> {
        if manifest.file_name().is_none_or(|name| name != ROOT) {
            return Err(invalid("expected project.fcmod"));
        }
        let folder = std::path::absolute(manifest)?.parent().ok_or_else(|| invalid("missing folder"))?.canonicalize()?;
        let bytes = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
        let writer = match revisions::writer(&folder) {
            Ok(lock) => Some(lock),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem) => None,
            Err(e) => return Err(e),
        };
        // A writer could have published between the first read and lock acquisition.
        let bytes = if writer.is_some() { read_member(&folder.join(ROOT), MANIFEST_LIMIT)? } else { bytes };
        let document = assemble(&folder, &serde_json::from_slice(&bytes)?, &folder.join(ROOT))?;
        let raw = ProjectRecords::split(&document).map_err(invalid)?;
        Ok((Self { folder, writer, revision: digest(&bytes), raw }, document))
    }

    /// Reopening the same document transfers its lease after the replacement snapshot
    /// has opened successfully. Two snapshots can never publish through the same lease.
    pub fn inherit_writer(&mut self, previous: &mut Self) {
        if self.folder == previous.folder && self.writer.is_none() {
            self.writer = previous.writer.take();
        }
    }

    pub fn is_manifest(&self, manifest: &Path) -> bool {
        manifest.file_name().is_some_and(|name| name == ROOT)
            && std::path::absolute(manifest).ok().and_then(|p| p.parent()?.canonicalize().ok()).is_some_and(|folder| folder == self.folder)
    }

    pub fn writable(&self) -> bool {
        self.writer.is_some()
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn save(&mut self, before: &[u8], after: &[u8]) -> io::Result<()> {
        self.save_from(&self.revision.clone(), before, after)
    }

    /// Merge an edit based on an earlier snapshot, publishing against the open HEAD.
    pub fn save_from(&mut self, base: &str, before: &[u8], after: &[u8]) -> io::Result<()> {
        if !self.writable() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "project folder is read-only"));
        }
        let next = self.merge(base, before, after)?;
        // Keep the new raw baseline only after the manifest has been published.
        let rev = revisions::save_locked(&self.folder, &self.revision, &next, revisions::publish)?;
        self.raw = next;
        self.revision = rev;
        Ok(())
    }

    fn merge(&self, base: &str, before: &[u8], after: &[u8]) -> io::Result<ProjectRecords> {
        let before = ProjectRecords::split(before).map_err(invalid)?;
        let after = ProjectRecords::split(after).map_err(invalid)?;
        if base == self.revision {
            return overlay::edited(&self.raw, &before, after);
        }
        let manifest = super::recovery::at(&self.folder, base)?;
        let raw = super::read_records(&self.folder, &manifest)?;
        overlay::edited(&raw, &before, after)
    }

    pub fn export(&self, before: &[u8], after: &[u8], destination: &Path) -> io::Result<()> {
        self.export_from(&self.revision, before, after, destination)
    }

    pub fn export_from(&self, base: &str, before: &[u8], after: &[u8], destination: &Path) -> io::Result<()> {
        if destination.extension().is_none_or(|ext| ext != "fcproj") {
            return Err(invalid("expected a new .fcproj destination"));
        }
        super::outside_folder(&self.folder, destination)?;
        let data = encode_at(&self.folder, self.merge(base, before, after)?, destination)?;
        crate::atomic_write_new(destination, &data)
    }
}
