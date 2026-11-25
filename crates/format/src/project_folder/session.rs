//! A manual editing snapshot. The OS lock stays attached to the open document;
//! immutable records still allow a consistent reader while another writer publishes.
use super::{MANIFEST_LIMIT, ProjectRecords, ROOT, assemble, digest, encode_at, invalid, overlay, read_member, revisions};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Source of opaque fields for a typed edit. A recovered draft can contain records that
/// were never manually published (including clips brought back by Undo).
#[derive(Clone, Debug, PartialEq)]
pub enum EditOrigin {
    Revision(String),
    Recovered(Arc<[u8]>),
}

impl EditOrigin {
    fn merge(&self, folder: &Path, current: Option<(&str, &ProjectRecords)>, before: &[u8], after: &[u8]) -> io::Result<ProjectRecords> {
        let before = ProjectRecords::split(before).map_err(invalid)?;
        let after = ProjectRecords::split(after).map_err(invalid)?;
        let raw = match self {
            Self::Revision(base) => {
                if let Some((head, raw)) = current
                    && base == head
                {
                    return overlay::edited(raw, &before, after);
                }
                super::read_records(folder, &super::recovery::at(folder, base)?)?
            }
            Self::Recovered(bytes) => ProjectRecords::split(bytes).map_err(invalid)?,
        };
        overlay::edited(&raw, &before, after)
    }

    /// Produce a standalone document on a recovery worker without transferring the writer lease.
    pub fn checkpoint(&self, folder: &Path, before: &[u8], after: &[u8], destination: &Path) -> io::Result<Vec<u8>> {
        encode_at(folder, self.merge(folder, None, before, after)?, destination)
    }
}

/// Move a checkpoint document between the journal, original manifest and independent export.
pub fn relocate_document(bytes: &[u8], from: &Path, to: &Path) -> io::Result<Vec<u8>> {
    crate::decode(bytes).map_err(invalid)?;
    let mut records = ProjectRecords::split(bytes).map_err(invalid)?;
    super::paths::rebase(&mut records, from, to)?;
    super::checked_bytes(&records.into_document().map_err(invalid)?)
}

pub struct FolderSnapshot {
    folder: PathBuf,
    writer: Option<File>,
    shared: bool,
    claim: Option<super::shared::Claim>,
    cursor: u64,
    revision: String,
    raw: ProjectRecords,
}

impl FolderSnapshot {
    pub fn open(manifest: &Path) -> io::Result<(Self, Vec<u8>)> {
        Self::open_mode(manifest, false)
    }

    pub fn open_shared(manifest: &Path) -> io::Result<(Self, Vec<u8>)> {
        Self::open_mode(manifest, true)
    }

    fn open_mode(manifest: &Path, shared: bool) -> io::Result<(Self, Vec<u8>)> {
        if manifest.file_name().is_none_or(|name| name != ROOT) {
            return Err(invalid("expected project.fcmod"));
        }
        let folder = std::path::absolute(manifest)?.parent().ok_or_else(|| invalid("missing folder"))?.canonicalize()?;
        let bytes = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
        let writer = if shared {
            None
        } else {
            match revisions::writer(&folder) {
                Ok(lock) => Some(lock),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem) => None,
                Err(e) => return Err(e),
            }
        };
        // A writer could have published between the first read and lock acquisition.
        let bytes = if writer.is_some() { read_member(&folder.join(ROOT), MANIFEST_LIMIT)? } else { bytes };
        let document = assemble(&folder, &serde_json::from_slice(&bytes)?, &folder.join(ROOT))?;
        let raw = ProjectRecords::split(&document).map_err(invalid)?;
        Ok((Self { folder, writer, shared, claim: None, cursor: 0, revision: digest(&bytes), raw }, document))
    }

    /// Reopening the same document transfers its lease after the replacement snapshot
    /// has opened successfully. Two snapshots can never publish through the same lease.
    pub fn inherit_writer(&mut self, previous: &mut Self) {
        if !self.shared && !previous.shared && self.folder == previous.folder && self.writer.is_none() {
            self.writer = previous.writer.take();
        }
    }

    pub fn is_shared(&self) -> bool {
        self.shared
    }

    pub fn scope(&self) -> Option<&super::BinScope> {
        self.claim.as_ref().map(|c| &c.scope)
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    pub fn advance_cursor(&mut self, next: u64) {
        self.cursor = self.cursor.max(next);
    }

    /// Lock all records as a unit, then install the snapshot that was verified under those locks.
    pub fn checkout_bin(&mut self, bin: u64, recovery: Option<(&str, &super::BinScope, &[u8], u64)>) -> io::Result<Vec<u8>> {
        if !self.shared || self.claim.is_some() {
            return Err(invalid("checkout requires an unclaimed shared session"));
        }
        let (claim, rev, raw) = super::shared::Claim::acquire(&self.folder, bin, recovery.map(|(h, s, _, _)| (h, s)))?;
        let (bytes, cursor) = if let Some((_, _, bytes, cursor)) = recovery {
            let draft = ProjectRecords::split(bytes).map_err(invalid)?;
            let merged = claim.project_draft(&raw, &draft, cursor)?;
            (encode_at(&self.folder, merged, &self.folder.join(ROOT))?, cursor)
        } else {
            let copy = super::shared::copy_records(&raw);
            (encode_at(&self.folder, copy, &self.folder.join(ROOT))?, claim.scope.start)
        };
        self.raw = raw;
        self.cursor = cursor;
        self.revision = rev;
        self.claim = Some(claim);
        Ok(bytes)
    }

    pub fn refresh_claim(&mut self) -> io::Result<Vec<u8>> {
        let claim = self.claim.as_ref().ok_or_else(|| invalid("no active checkout"))?;
        let bytes = read_member(&self.folder.join(ROOT), MANIFEST_LIMIT)?;
        let m = serde_json::from_slice(&bytes)?;
        claim.scope.verify(&super::recovery::at(&self.folder, &self.revision)?, &m)?;
        let doc = assemble(&self.folder, &m, &self.folder.join(ROOT))?;
        self.raw = ProjectRecords::split(&doc).map_err(invalid)?;
        self.revision = digest(&bytes);
        Ok(doc)
    }

    pub fn is_manifest(&self, manifest: &Path) -> bool {
        manifest.file_name().is_some_and(|name| name == ROOT)
            && std::path::absolute(manifest).ok().and_then(|p| p.parent()?.canonicalize().ok()).is_some_and(|folder| folder == self.folder)
    }

    pub fn directory(&self) -> &Path {
        &self.folder
    }

    pub fn writable(&self) -> bool {
        self.writer.is_some() || self.claim.is_some()
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn save(&mut self, before: &[u8], after: &[u8]) -> io::Result<()> {
        self.save_from(&self.revision.clone(), before, after)
    }

    /// Merge an edit based on an earlier snapshot, publishing against the open HEAD.
    pub fn save_from(&mut self, base: &str, before: &[u8], after: &[u8]) -> io::Result<()> {
        self.save_edit(&EditOrigin::Revision(base.to_owned()), before, after)
    }

    pub fn save_edit(&mut self, origin: &EditOrigin, before: &[u8], after: &[u8]) -> io::Result<()> {
        self.save_edit_with_origin(origin, before, after).map(|_| ())
    }

    /// Return the raw baseline for subsequent local edits and Undo after publishing.
    pub fn save_edit_with_origin(&mut self, origin: &EditOrigin, before: &[u8], after: &[u8]) -> io::Result<EditOrigin> {
        if !self.writable() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "project folder is read-only"));
        }
        let next = origin.merge(&self.folder, Some((&self.revision, &self.raw)), before, after)?;
        // Keep the new raw baseline only after the manifest has been published.
        let (rev, origin) = if let Some(claim) = &self.claim {
            // Undo keeps the local editor's view of its peers. Prepare that baseline
            // before the manifest commit so there is no fallible work afterward.
            let pinned = encode_at(&self.folder, super::shared::copy_records(&next), &self.folder.join(ROOT))?;
            (claim.publish(&self.folder, &self.revision, &next, self.cursor)?, EditOrigin::Recovered(pinned.into()))
        } else {
            let rev = revisions::save_locked(&self.folder, &self.revision, &next, revisions::publish)?;
            (rev.clone(), EditOrigin::Revision(rev))
        };
        self.raw = next;
        self.revision = rev;
        Ok(origin)
    }

    pub fn export(&self, before: &[u8], after: &[u8], destination: &Path) -> io::Result<()> {
        self.export_from(&self.revision, before, after, destination)
    }

    pub fn export_from(&self, base: &str, before: &[u8], after: &[u8], destination: &Path) -> io::Result<()> {
        self.export_edit(&EditOrigin::Revision(base.to_owned()), before, after, destination)
    }

    pub fn export_edit(&self, origin: &EditOrigin, before: &[u8], after: &[u8], destination: &Path) -> io::Result<()> {
        if destination.extension().is_none_or(|ext| ext != "fcproj") {
            return Err(invalid("expected a new .fcproj destination"));
        }
        super::outside_folder(&self.folder, destination)?;
        let data = encode_at(&self.folder, origin.merge(&self.folder, Some((&self.revision, &self.raw)), before, after)?, destination)?;
        crate::atomic_write_new(destination, &data)
    }
}
