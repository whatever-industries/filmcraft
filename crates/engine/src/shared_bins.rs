//! Headless bin checkout. Changing scopes installs a fresh document epoch so a previous
//! bin's Undo history cannot be replayed against the new owner.
use crate::{
    EngineError, Result, Session,
    folder_edit::{EditingFolder, FolderBase},
    folder_recovery::FolderJournal,
};
use filmcraft_format::project_folder::{EditOrigin, FolderSnapshot, ROOT};
use filmcraft_project::{BinEntry, BinId, Project};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

impl Session {
    pub fn enable_shared_bins(&mut self, data: &Path) -> Result<()> {
        self.enable_folder_recovery(data)?;
        self.shared_bins = true;
        Ok(())
    }

    pub(super) fn shared_command(&self, id: &str, p: &Value) -> Result<()> {
        let Some(folder) = self.editing_folder.as_ref().filter(|f| f.snapshot.is_shared()) else { return Ok(()) };
        if self.editing_bins()
            && matches!(
                id,
                "file.saveAll"
                    | "file.saveAs"
                    | "file.discardRecovery"
                    | "file.recover"
                    | "file.open"
                    | "file.closeProject"
                    | "file.newProject"
                    | "file.closeAllProjects"
                    | "sequence.close"
                    | "sequence.closeOthers"
                    | "project.select"
                    | "project.selectAll"
                    | "project.deselectAll"
                    | "source.open"
                    | "source.close"
                    | "source.closeAll"
                    | "source.setPlayhead"
                    | "timeline.select"
                    | "playhead.set"
                    | "playhead.step"
                    | "playhead.start"
                    | "playhead.end"
                    | "playhead.stepForward"
                    | "playhead.stepBack"
                    | "playhead.stepForward5"
                    | "playhead.stepBack5"
                    | "playhead.nextEdit"
                    | "playhead.prevEdit"
            )
        {
            return Ok(());
        }
        match id {
            "project.inspect"
            | "project.view.get"
            | "project.view.set"
            | "project.referenceClips"
            | "project.binHistory"
            | "sequence.inspect"
            | "source.inspect"
            | "sequence.open"
            | "file.saveCopy"
            | "file.recoveryList"
            | "file.autoSaveStatus"
            | "project.checkoutBin"
            | "project.releaseBin"
            | "project.refresh" => Ok(()),
            "file.open" | "file.revert" | "file.recover" | "file.closeProject" if !self.is_dirty() => Ok(()),
            "project.renameBin"
            | "project.removeFromBin"
            | "project.fileInBin"
            | "edit.label"
            | "metadata.set"
            | "edit.duplicate"
            | "project.reorderBin"
            | "timeline.move"
            | "timeline.trim"
            | "timeline.razor"
            | "timeline.delete"
            | "edit.clear"
            | "edit.rippleDelete"
            | "timeline.place"
            | "source.open"
            | "source.insert"
            | "source.setPlayhead"
            | "timeline.select"
            | "sequence.addEdit"
            | "sequence.addEditAllTracks"
            | "edit.undo"
            | "edit.redo"
            | "file.save"
            | "file.autoSaveNow"
                if folder.snapshot.scope().is_some() =>
            {
                Ok(())
            }
            _ if crate::commands::reports_only(id, p) => Ok(()),
            _ => Err(error(format!("{id} is unavailable in this checkout; save or release the bin to switch documents"))),
        }
    }

    pub(super) fn prepare_scoped_edit(&self, p: &mut Project) {
        if let Some(f) = self.editing_folder.as_ref().filter(|f| f.snapshot.scope().is_some()) {
            p.next_id = f.snapshot.cursor();
        }
    }

    pub(super) fn validate_scoped_edit(&mut self, p: &Project) -> Result<()> {
        if let Some(f) = &mut self.editing_folder
            && f.snapshot.is_shared()
        {
            f.snapshot.scope().ok_or_else(|| error("check out a bin before editing"))?.validate_edit(&self.project, p).map_err(error)?;
            f.snapshot.advance_cursor(p.next_id);
        }
        Ok(())
    }

    pub(crate) fn change_checkout(&mut self, id: &str, p: &Value) -> Result<Value> {
        if self.editing_bins() {
            return match id {
                "project.checkoutBin" => self.open_desktop_bin(p["bin"].as_u64().ok_or_else(|| error("expected a bin ID"))?),
                "project.releaseBin" => self.close_desktop_bin(p["bin"].as_u64().or(self.focused_bin()).ok_or_else(|| error("choose an open bin"))?),
                _ => Err(error("close and reopen the bin to refresh")),
            };
        }
        let f = self.editing_folder.as_ref().filter(|f| f.snapshot.is_shared()).ok_or_else(|| error("open a project folder with --shared-bins"))?;
        let discard = p.get("discard").map(|v| v.as_bool().ok_or_else(|| error("discard must be a boolean"))).transpose()?.unwrap_or(false);
        if self.is_dirty() && (!discard || id == "project.refresh") {
            return Err(error("save this checkout or explicitly discard its draft before switching"));
        }
        let manifest = f.snapshot.directory().join(ROOT);
        let path = manifest.to_str().ok_or_else(|| error("non-UTF-8 project path"))?;
        let current = f.snapshot.scope().map(|s| s.bin);
        let selected = match id {
            "project.checkoutBin" => Some(p["bin"].as_u64().ok_or_else(|| error("expected a bin ID"))?),
            "project.releaseBin" => None,
            "project.refresh" => current,
            _ => return Err(error("unknown checkout command")),
        };
        // Refreshing the existing claim keeps its OS locks continuously held.
        let (snapshot, doc, journal) = if selected.is_some() && current == selected {
            let mut f = self.editing_folder.take().ok_or_else(|| error("no open folder"))?;
            let doc = match f.snapshot.refresh_claim() {
                Ok(bytes) => bytes,
                Err(e) => {
                    self.editing_folder = Some(f);
                    return Err(error(e));
                }
            };
            if discard
                && let Some(journal) = &f.journal
                && let Err(e) = journal.clear()
            {
                self.editing_folder = Some(f);
                return Err(e);
            }
            (f.snapshot, doc, f.journal)
        } else {
            let (mut snapshot, mut doc) = FolderSnapshot::open_shared(&manifest).map_err(error)?;
            let journal = if let Some(bin) = selected {
                doc = snapshot.checkout_bin(bin, None).map_err(error)?;
                Some(FolderJournal::start(self.folder_recovery.as_ref().ok_or_else(|| error("missing recovery directory"))?, &snapshot, None)?)
            } else {
                None
            };
            if let Some(f) = &self.editing_folder
                && let Some(j) = &f.journal
            {
                j.clear()?;
            }
            (snapshot, doc, journal)
        };
        self.editing_folder.take();
        crate::commands::open_bytes(self, path, &doc)?;
        let origin = EditOrigin::Recovered(doc.into());
        if let Some(scope) = snapshot.scope() {
            Arc::make_mut(&mut self.project).next_id = snapshot.cursor();
            if self.state.active_sequence.is_none_or(|item| !scope.sequences.contains(&item.0))
                && let Some(&seq) = scope.sequences.first()
            {
                let seq = filmcraft_project::ItemId(seq);
                self.state.active_sequence = Some(seq);
                if !self.state.open_sequences.contains(&seq) {
                    self.state.open_sequences.push(seq);
                }
            }
        }
        let base = Arc::new(FolderBase { origin, project: self.project.clone(), view: self.project_view() });
        self.editing_folder = Some(EditingFolder { snapshot, baseline: base, journal });
        Ok(crate::commands::inspect_project(self))
    }

    pub(crate) fn reorder_bin(&mut self, p: &Value) -> Result<Value> {
        let bin = BinId(p["bin"].as_u64().ok_or_else(|| error("expected a bin ID"))?);
        if self.editing_folder.as_ref().and_then(|f| f.snapshot.scope()).is_none_or(|s| s.bin != bin.0) {
            return Err(error("reordering requires checkout of this bin"));
        }
        let order: Vec<u64> = serde_json::from_value(p["order"].clone()).map_err(error)?;
        self.edit("Reorder bin", |project, _| {
            let target = project.root.find_bin_mut(bin).ok_or_else(|| error("missing bin"))?;
            if order.len() != target.children.len() {
                return Err(error("order must contain every entry once"));
            }
            let mut entries: std::collections::BTreeMap<_, _> = std::mem::take(&mut target.children)
                .into_iter()
                .map(|e| {
                    let id = match &e {
                        BinEntry::Item(i) => i.0,
                        BinEntry::Bin(b) => b.id.0,
                    };
                    (id, e)
                })
                .collect();
            for id in order {
                target.children.push(entries.remove(&id).ok_or_else(|| error("unknown or repeated bin entry"))?);
            }
            Ok(())
        })?;
        Ok(json!({"bin":bin.0}))
    }
}
