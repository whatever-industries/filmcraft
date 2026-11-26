//! Capture the editor's published bases before moving the retention scan off the UI thread.
use super::{EditingFolder, History, Session, error};
use crate::{Result, autosave};
use filmcraft_format::project_folder::{EditOrigin, ROOT, checkpoint::Checkpoint, retention};
use std::{
    collections::BTreeSet,
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

impl Session {
    pub fn prepare_retention_preview(&self, keep_latest: NonZeroUsize) -> Result<impl FnOnce() -> Result<retention::Preview> + Send + 'static + use<>> {
        let active = self.editing_folder.as_ref().ok_or_else(|| error("open a modular project first"))?;
        let folder = active.snapshot.directory().to_owned();
        let recovery = self.folder_recovery.as_deref().map(autosave::recovery_root);
        let mut readers = BTreeSet::new();
        let mut capture = |edit: &EditingFolder, history: &History| {
            readers.insert(edit.snapshot.revision().to_owned());
            for base in std::iter::once(&edit.baseline).chain(history.undo.iter().chain(&history.redo).filter_map(|(_, state, _)| state.folder_base.as_ref())) {
                if let EditOrigin::Revision(hash) = &base.origin {
                    readers.insert(hash.clone());
                }
            }
        };
        capture(active, &self.history);
        for slot in self.bin_editors.parked.values() {
            capture(&slot.folder, &slot.history);
        }
        let readers = readers.into_iter().collect::<Vec<_>>();
        Ok(move || {
            let drafts = recovery.as_deref().map(|root| local_drafts(root, &folder)).transpose().map_err(error)?.unwrap_or_default();
            let mut report = retention::preview(&folder, keep_latest, &readers, &drafts).map_err(error)?;
            report.protection_scope = if recovery.is_some() {
                "Open editor, parked bins, Undo/Redo bases and matching checkpoint descriptors in this profile. Other readers and profiles may exist."
            } else {
                "Open editor, parked bins and Undo/Redo bases. This session has no recovery profile; other readers and drafts may exist."
            };
            Ok(report)
        })
    }
}

// Descriptor publication is atomic. Reading a live journal conservatively protects its base too;
// claiming recovery ownership here would hide active checkpoints and interfere with actual recovery.
fn local_drafts(root: &Path, folder: &Path) -> io::Result<Vec<PathBuf>> {
    match root.symlink_metadata() {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(io::Error::other("invalid folder recovery directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let manifest = folder.join(ROOT);
    let mut drafts = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        match Checkpoint::read(&dir) {
            Ok(c) if c.source.manifest == manifest => drafts.push(dir),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io::Error::other(format!("cannot inspect checkpoint in {}: {e}", dir.display()))),
        }
    }
    Ok(drafts)
}
