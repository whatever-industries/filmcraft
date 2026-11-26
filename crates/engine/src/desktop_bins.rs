//! A local desktop holds one lease, Undo history and recovery worker per writable bin.
//! The active bin uses the existing editor fields. Parking it keeps its lease and worker alive.
use crate::{
    EngineError, History, Result, Session,
    autosave::AutosaveConfig,
    folder_edit::{EditingFolder, FolderBase},
    folder_recovery::FolderJournal,
};
use filmcraft_format::project_folder::{EditOrigin, FolderSnapshot, ROOT};
use filmcraft_project::{BinId, ItemId};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc};

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

#[derive(Default)]
pub(super) struct BinEditors {
    focus: Option<u64>,
    parked: BTreeMap<Option<u64>, BinEditor>,
}

struct BinEditor {
    folder: EditingFolder,
    history: History,
    dirty: bool,
}

impl Session {
    pub fn desktop_project_editor(config: AutosaveConfig) -> Result<Self> {
        let mut session = Self::local_project_editor(config)?;
        session.shared_bins = true;
        Ok(session)
    }

    pub fn editing_bins(&self) -> bool {
        self.shared_bins && self.mixed_config.is_some() && self.editing_folder.as_ref().is_some_and(|f| f.snapshot.is_shared())
    }

    pub fn bin_access(&self, bin: u64) -> Option<bool> {
        if !self.editing_bins() {
            return None;
        }
        if self.bin_editors.focus == Some(bin) {
            return self.editing_folder.as_ref().map(|f| f.snapshot.writable());
        }
        self.bin_editors.parked.get(&Some(bin)).map(|s| s.folder.snapshot.writable())
    }

    pub fn focused_bin(&self) -> Option<u64> {
        self.bin_editors.focus
    }

    fn swap_bin(&mut self, next: Option<u64>, slot: BinEditor) {
        crate::trim::commit(self);
        self.history.merge_key = None;
        self.sync_folder_recovery();
        let dirty = self.is_dirty();
        let history = std::mem::replace(&mut self.history, slot.history);
        if let Some(folder) = self.editing_folder.replace(slot.folder) {
            self.bin_editors.parked.insert(self.bin_editors.focus, BinEditor { folder, history, dirty });
        }
        self.bin_editors.focus = next;
        self.bump();
        self.saved_revision = if slot.dirty { 0 } else { self.revision };
    }

    fn focus_open_bin(&mut self, next: Option<u64>) -> Result<()> {
        if self.bin_editors.focus != next {
            let slot = self.bin_editors.parked.remove(&next).ok_or_else(|| error("open the bin before editing it"))?;
            self.swap_bin(next, slot);
        }
        Ok(())
    }

    pub(super) fn open_desktop_bin(&mut self, bin: u64) -> Result<Value> {
        if bin == self.project.root.id.0 {
            self.focus_open_bin(None)?;
            return Ok(json!({"bin":bin,"readOnly":true}));
        }
        if self.bin_access(bin).is_some() {
            self.focus_open_bin(Some(bin))?;
        } else {
            let manifest = self.editing_folder.as_ref().ok_or_else(|| error("open a project folder"))?.snapshot.directory().join(ROOT);
            let (mut snapshot, published) = FolderSnapshot::open_shared(&manifest).map_err(error)?;
            let bytes = match snapshot.checkout_bin(bin, None) {
                Ok(doc) => doc,
                Err(e)
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem) =>
                {
                    published
                }
                Err(e) => return Err(error(e)),
            };
            let loaded = filmcraft_format::decode(&bytes).map_err(error)?;
            if loaded.project.root.find_bin(BinId(bin)).is_none() {
                return Err(error("missing bin"));
            }
            let view = self.project_view();
            let base = Arc::new(FolderBase { origin: EditOrigin::Recovered(bytes.into()), project: Arc::new(loaded.project), view });
            let journal = if snapshot.writable() {
                Some(FolderJournal::start(
                    self.folder_recovery.as_ref().ok_or_else(|| error("missing recovery directory"))?,
                    &snapshot,
                    self.mixed_config.as_ref(),
                )?)
            } else {
                None
            };
            self.install_bin(bin, EditingFolder { snapshot, baseline: base, journal }, false)?;
        }
        Ok(json!({"bin":bin,"readOnly":self.bin_access(bin)!=Some(true)}))
    }

    pub(super) fn restore_desktop_bin(
        &mut self,
        snapshot: FolderSnapshot,
        bytes: Vec<u8>,
        mut project: filmcraft_project::Project,
        journal: FolderJournal,
    ) -> Result<()> {
        let bin = snapshot.scope().ok_or_else(|| error("missing recovery bin"))?.bin;
        let path = snapshot.directory().join(ROOT);
        if !self.editing_folder.as_ref().is_some_and(|f| f.snapshot.is_manifest(&path)) {
            self.open_folder(path.to_str().ok_or_else(|| error("invalid project path"))?)?;
        }
        project.next_id = snapshot.cursor();
        let baseline = Arc::new(FolderBase { origin: EditOrigin::Recovered(bytes.into()), project: Arc::new(project), view: self.project_view() });
        self.install_bin(bin, EditingFolder { snapshot, baseline, journal: Some(journal) }, true)?;
        self.flush_folder_recovery()?;
        Ok(())
    }

    fn install_bin(&mut self, bin: u64, folder: EditingFolder, dirty: bool) -> Result<()> {
        // Bring in published peer edits while retaining every local checkout.
        let mut visible = folder.baseline.project.clone();
        for f in self.editing_folder.iter().chain(self.bin_editors.parked.values().map(|slot| &slot.folder)) {
            if let Some(scope) = f.snapshot.scope() {
                visible = Arc::new(scope.project_onto(&visible, &self.project).map_err(error)?);
            }
        }
        if self.focused_bin() == Some(bin) {
            self.focus_open_bin(None)?;
        }
        self.bin_editors.parked.remove(&Some(bin));
        self.swap_bin(Some(bin), BinEditor { folder, history: History { limit: 200, ..Default::default() }, dirty });
        self.project = visible;
        Ok(())
    }

    pub(super) fn route_bin_edit(&mut self, id: &str, p: &Value) -> Result<()> {
        if !self.editing_bins() {
            return Ok(());
        }
        if id == "project.pasteFromHistory"
            && p["epoch"].as_u64() == Some(self.document_epoch)
            && let Some(bin) = p["bin"].as_u64()
            && self.bin_access(bin) == Some(true)
        {
            return self.focus_open_bin(Some(bin));
        }
        let owner = if matches!(
            id,
            "project.renameBin" | "project.reorderBin" | "project.removeFromBin" | "project.fileInBin" | "metadata.set" | "edit.label" | "edit.duplicate"
        ) {
            p["bin"].as_u64()
        } else if id == "sequence.open" {
            p["item"].as_u64().filter(|&n| self.project.sequence(ItemId(n)).is_some()).and_then(|n| self.project.root.parent_of(ItemId(n))).map(|b| b.0)
        } else if id.starts_with("timeline.")
            || matches!(id, "edit.clear" | "edit.rippleDelete" | "sequence.addEdit" | "sequence.addEditAllTracks" | "source.insert")
        {
            self.state.active_sequence.and_then(|s| self.project.root.parent_of(s)).map(|b| b.0)
        } else {
            None
        };
        if let Some(bin) = owner {
            self.open_desktop_bin(bin)?;
        }
        Ok(())
    }

    fn save_active_bin(&mut self) -> Result<()> {
        if self.is_dirty() {
            let path = self.path.clone().ok_or_else(|| error("missing project path"))?;
            self.save_folder(&path, true)
                .map_err(|e| error(format!("Bin {}: {e}", self.bin_editors.focus.map_or_else(|| "root".into(), |id| id.to_string()))))?;
        }
        self.flush_folder_recovery().map_err(|e| error(format!("Bin {:?}: {e}", self.focused_bin())))?;
        Ok(())
    }

    pub(super) fn save_open_bins(&mut self) -> Result<Value> {
        let focus = self.bin_editors.focus;
        let mut dirty: Vec<_> = self
            .bin_editors
            .parked
            .iter()
            .filter(|(_, b)| b.dirty || b.folder.journal.as_ref().is_some_and(|j| j.pending_cleanup))
            .filter_map(|(k, _)| *k)
            .collect();
        if (self.is_dirty() || self.editing_folder.as_ref().and_then(|f| f.journal.as_ref()).is_some_and(|j| j.pending_cleanup))
            && let Some(bin) = focus
        {
            dirty.push(bin);
        }
        if dirty.len() > 1 {
            (dirty, _) = self.compose_bins()?;
        }
        let result = dirty.iter().try_for_each(|bin| {
            self.focus_open_bin(Some(*bin))?;
            self.save_active_bin()
        });
        self.focus_open_bin(focus)?;
        result?;
        Ok(json!({"savedBins":dirty}))
    }

    pub(super) fn close_desktop_bin(&mut self, bin: u64) -> Result<Value> {
        if self.bin_access(bin).is_none() {
            return Err(error("bin is not open"));
        }
        let previous = self.bin_editors.focus;
        self.focus_open_bin(Some(bin))?;
        self.save_active_bin()?;
        self.focus_open_bin(previous.filter(|n| *n != bin))?;
        self.bin_editors.parked.remove(&Some(bin));
        Ok(json!({"closedBin":bin}))
    }

    pub(super) fn history_document(&self, mut document: Vec<u8>, typed: &[u8]) -> Result<Vec<u8>> {
        for parked in self.bin_editors.parked.values().filter(|slot| slot.folder.snapshot.scope().is_some()) {
            let f = &parked.folder;
            document = f.snapshot.overlay_history_details(&document, &f.baseline.origin, &f.baseline.typed(), typed).map_err(error)?;
        }
        Ok(document)
    }

    // An unpublished sequence can use a source created by a different checkout.
    // Determine a valid publication order against one snapshot before saving any bin.
    fn compose_bins(&self) -> Result<(Vec<u64>, Vec<u8>)> {
        let active = self.editing_folder.as_ref().ok_or_else(|| error("open a project folder"))?;
        let (_, mut doc) = FolderSnapshot::open_shared(&active.snapshot.directory().join(ROOT)).map_err(error)?;
        let view = self.project_view();
        let mut pending = BTreeMap::new();
        for f in self
            .bin_editors
            .parked
            .values()
            .filter(|b| b.dirty || b.folder.journal.as_ref().is_some_and(|j| j.pending_cleanup))
            .map(|b| &b.folder)
            .chain(self.editing_folder.iter().filter(|f| self.is_dirty() || f.journal.as_ref().is_some_and(|j| j.pending_cleanup)))
        {
            let bin = f.snapshot.scope().ok_or_else(|| error("expected a bin checkout"))?.bin;
            let scoped = f.scoped_project(&self.project)?;
            pending.insert(bin, (f, f.baseline.typed(), filmcraft_format::encode_with_view(&scoped, Some(&view), false)));
        }
        let mut order = Vec::new();
        while !pending.is_empty() {
            let mut problem = None;
            let ready = pending.iter().find_map(|(&id, (f, before, after))| match f.snapshot.overlay_checkout(&doc, &f.baseline.origin, before, after) {
                Ok(next) => Some((id, next)),
                Err(e) => {
                    problem.get_or_insert_with(|| error(format!("Bin {id}: {e}")));
                    None
                }
            });
            let (id, next) = ready.ok_or_else(|| problem.unwrap_or_else(|| error("could not compose bin edits")))?;
            pending.remove(&id);
            order.push(id);
            doc = next;
        }
        Ok((order, doc))
    }

    pub(super) fn export_bins(&self, path: &Path) -> Result<Value> {
        let active = self.editing_folder.as_ref().ok_or_else(|| error("open a project folder"))?;
        let (_, doc) = self.compose_bins()?;
        active.snapshot.export_document(&doc, path).map_err(error)?;
        Ok(json!({"path":path}))
    }
}

#[cfg(test)]
mod tests;
