//! Capture the editor's published bases before moving the retention scan off the UI thread.
use super::{EditingFolder, History, Session, error};
use crate::Result;
use filmcraft_format::project_folder::{EditOrigin, retention};
use std::{collections::BTreeSet, num::NonZeroUsize};

impl Session {
    pub fn prepare_retention_preview(&self, keep_latest: NonZeroUsize) -> Result<impl FnOnce() -> Result<retention::Preview> + Send + 'static + use<>> {
        let active = self.editing_folder.as_ref().ok_or_else(|| error("open a modular project first"))?;
        let folder = active.snapshot.directory().to_owned();
        let recovery = self.folder_recovery.as_deref().map(crate::autosave::recovery_root);
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
            let drafts = match &recovery {
                Some(root) => retention::draft_sessions(root, &folder).map_err(error)?.unwrap_or_default(),
                None => Vec::new(),
            };
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
