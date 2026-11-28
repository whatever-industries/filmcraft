//! Local project-folder backing for the editor's ordinary open and save commands.
use crate::{EngineError, Result, Session};
use filmcraft_format::project_folder::{Departure, DepartureCut, FolderSnapshot, TransferSave};
use filmcraft_project::{BinId, ItemId, Project, ProjectView};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
enum CapturedCut {
    Working(Arc<Project>),
    Archived(String),
}

#[derive(Clone)]
pub(crate) struct CapturedMove {
    destination: BinId,
    saved_source: String,
    cut: CapturedCut,
}

type Departures = std::collections::BTreeMap<(BinId, ItemId), CapturedMove>;

pub(crate) struct EditingFolder {
    pub departures: Departures,
    pub cleanup: Option<BinId>,
    pub snapshot: FolderSnapshot,
    pub project: Arc<Project>,
    pub view: ProjectView,
}

impl EditingFolder {
    // Captures share the immutable pre-drag project. Only the moved sequence
    // and its transitive sources are serialized when a bin is actually saved.
    pub(crate) fn capture_moves(&mut self, before: &Arc<Project>, after: &Project) {
        if before.root == after.root {
            return;
        }
        for item in before.sequences() {
            if let (Some(source), Some(destination)) = (before.root.parent_of(item.id), after.root.parent_of(item.id))
                && source != destination
            {
                // A round trip before either save cancels the removal. Only a
                // previously published source can have an outstanding removal.
                self.departures.remove(&(destination, item.id));
                if self.project.root.parent_of(item.id) == Some(source) {
                    self.departures.insert(
                        (source, item.id),
                        CapturedMove { destination, saved_source: self.snapshot.revision().to_owned(), cut: CapturedCut::Working(before.clone()) },
                    );
                } else {
                    for ((_, seq), capture) in &mut self.departures {
                        if *seq == item.id && capture.destination == source {
                            capture.destination = destination;
                        }
                    }
                }
            }
        }
    }

    fn transfers<'a>(captures: &'a Departures, working: &Project, bin: Option<BinId>) -> TransferSave<'a> {
        let departures = captures
            .iter()
            .filter_map(|(&(source, sequence), capture)| {
                let owner = working.root.parent_of(sequence);
                if owner == Some(source) || bin.is_some_and(|b| b != source && (b != capture.destination || Some(b) != owner)) {
                    return None;
                }
                let cut = match &capture.cut {
                    CapturedCut::Working(project) => DepartureCut::Working(project),
                    CapturedCut::Archived(hash) => DepartureCut::Archived(hash),
                };
                Some(Departure { sequence, source, destination: capture.destination, saved_source: &capture.saved_source, cut })
            })
            .collect();
        let active_move_bases = captures.iter().filter(|((source, _), _)| bin.is_some_and(|b| b != *source)).map(|(_, c)| c.saved_source.clone()).collect();
        TransferSave { copies: Vec::new(), departures, selected_bin: bin.map(|id| id.0), active_move_bases }
    }
}

pub(crate) fn is_manifest(path: &str) -> bool {
    Path::new(path).file_name().is_some_and(|name| name == filmcraft_format::project_folder::ROOT)
}

impl Session {
    /// Enable explicit project-folder opens in a trusted local filesystem editor.
    pub fn local_folder_editor() -> Self {
        Self { allow_project_folders: true, ..Self::default() }
    }

    pub(crate) fn save_folder(&mut self, path: &str) -> Result<Value> {
        if self.warm_folder.is_some() {
            return self.save_warm(None);
        }
        let started = web_time::Instant::now();
        let view = self.project_view();
        let folder = self.editing_folder.as_mut().ok_or_else(|| EngineError::Other("open this project folder before saving it".into()))?;
        let moves = EditingFolder::transfers(&folder.departures, &self.project, None);
        let saved = folder
            .snapshot
            .save_transfers(&folder.project, &folder.view, &self.project, &view, moves)
            .map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        folder.departures.clear();
        folder.project = self.project.clone();
        folder.view = view;
        self.saved_revision = self.revision;
        self.previews_follow_path();
        self.note_recent_project();
        self.report_retention_warning(saved.maintenance_warning.as_deref());
        Ok(
            json!({"path":path,"bytes":saved.bytes,"ms":started.elapsed().as_secs_f64()*1000.0,"backup":null,"retentionWarning":saved.maintenance_warning,"folderRevision":saved.revision}),
        )
    }
}

mod media_check;
mod recovery;
mod scope;
pub(crate) mod warm;

pub(crate) fn available(s: &Session) -> std::result::Result<(), String> {
    s.folder_revision().map(|_| ()).ok_or_else(|| "open a local project folder first".into())
}

pub(crate) fn history_available(s: &Session) -> std::result::Result<(), String> {
    if s.detached_history_origin().is_some() { Ok(()) } else { available(s) }
}

fn bin_id(p: &Value, saved: &Project, working: &Project) -> Result<BinId> {
    let id = BinId(p.get("bin").and_then(Value::as_u64).ok_or_else(|| EngineError::Other("expected a numeric bin id".into()))?);
    if [saved, working].iter().all(|p| p.directory().parent(filmcraft_project::DirectoryEntry::Bin(id)).is_none()) {
        return Err(EngineError::Other(format!("unknown bin {}", id.0)));
    }
    Ok(id)
}

fn checkpoint(
    worker: Option<&crate::autosave::Persistence>,
    project: Option<Arc<Project>>,
    revision: u64,
    path: Option<String>,
    reviewed: Option<ItemId>,
) -> std::io::Result<()> {
    if let Some(worker) = worker {
        worker.replace_bin_journal(project, revision, path, reviewed).map_err(std::io::Error::other)?;
    }
    Ok(())
}

type Entry = (String, Arc<Project>, Option<String>);

fn rebased_history(history: &[Entry], saved: &Project, id: BinId, current: &Arc<Project>) -> Vec<Entry> {
    let mut result = Vec::new();
    let mut newer = current.clone();
    for (label, previous, origin) in history.iter().rev() {
        // Undo and redo store complete documents. Project each reachable state onto
        // the retained bin, dropping empty steps and incompatible historical moves.
        let rebased = if scope::status(previous, saved).bins.contains(&id) {
            let Ok(project) = scope::project_bin(previous, saved, id) else { continue };
            Arc::new(project)
        } else {
            previous.clone()
        };
        if scope::status(&newer, &rebased).dirty() {
            newer = rebased;
            result.push((label.clone(), newer.clone(), origin.clone()));
        }
    }
    result.reverse();
    result
}

impl Session {
    fn folder(&self) -> Result<&EditingFolder> {
        self.editing_folder.as_ref().ok_or_else(|| EngineError::Other("open a local project folder first".into()))
    }

    pub fn bin_has_move_removal(&self, bin: BinId) -> bool {
        self.editing_folder.as_ref().is_some_and(|f| f.departures.keys().any(|(source, seq)| *source == bin && self.project.root.parent_of(*seq) != Some(bin)))
    }

    pub fn dirty_folder_bins(&self) -> std::collections::BTreeSet<BinId> {
        let (project, _) = self.committed_state();
        if let Some(w) = &self.warm_folder {
            return w.edited_item.as_ref().filter(|(_, i)| project.item(i.id) != Some(i)).map(|(id, _)| *id).into_iter().collect();
        }
        if let Some(folder) = &self.editing_folder {
            return scope::status(&folder.project, project).bins;
        }
        // A detached relink draft retains a dirty owner until explicitly saved.
        self.persistence
            .as_ref()
            .and_then(|p| p.reviewed_source)
            .filter(|(epoch, _)| *epoch == self.document_epoch && self.is_dirty())
            .and_then(|(_, item)| crate::relink::review::owning_bin(project, item))
            .into_iter()
            .collect()
    }

    fn report_retention_warning(&mut self, warning: Option<&str>) {
        if let Some(warning) = warning {
            self.error_toast("project.recoveryRetention", format!("Saved changes. Bin backup cleanup needs attention. {warning}"));
        }
    }

    fn recovery_folder(&self) -> Result<(&Path, &str)> {
        let snapshot = &self.folder()?.snapshot;
        Ok((snapshot.directory().map_err(|e| EngineError::Other(e.to_string()))?, snapshot.revision()))
    }

    pub(crate) fn wait_for_recovery_prune(&mut self, _: &Value) -> Result<Value> {
        self.folder()?.snapshot.wait_for_retention().map_err(|e| EngineError::Other(e.to_string()))?;
        Ok(json!({"complete":true}))
    }

    pub(crate) fn recovery_retention(&mut self, _: &Value) -> Result<Value> {
        let (dir, _) = self.recovery_folder()?;
        Ok(json!(filmcraft_format::project_folder::retention_policy(dir).map_err(|e| EngineError::Other(e.to_string()))?))
    }

    pub(crate) fn configure_retention(&mut self, p: &Value) -> Result<Value> {
        let policy = serde_json::from_value(p.clone()).map_err(|e| EngineError::Other(e.to_string()))?;
        let (dir, rev) = self.recovery_folder()?;
        filmcraft_format::project_folder::configure_retention(dir, rev, policy).map_err(|e| EngineError::Other(e.to_string()))?;
        Ok(json!(policy))
    }

    pub(crate) fn preview_recovery_prune(&mut self, p: &Value) -> Result<Value> {
        let (dir, rev) = self.recovery_folder()?;
        let policy = match p.get("policy") {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| EngineError::Other(e.to_string()))?,
            None => filmcraft_format::project_folder::retention_policy(dir).map_err(|e| EngineError::Other(e.to_string()))?,
        };
        Ok(json!(filmcraft_format::project_folder::preview_retention(dir, rev, policy).map_err(|e| EngineError::Other(e.to_string()))?))
    }

    pub(crate) fn keep_recovery_version(&mut self, p: &Value) -> Result<Value> {
        let version = p["revision"].as_str().ok_or_else(|| EngineError::Other("expected a recovery revision".into()))?;
        let keep = p["keep"].as_bool().ok_or_else(|| EngineError::Other("expected a keep flag".into()))?;
        let (dir, rev) = self.recovery_folder()?;
        filmcraft_format::project_folder::keep_version(dir, rev, version, keep).map_err(|e| EngineError::Other(e.to_string()))?;
        Ok(json!({"revision":version,"keep":keep}))
    }

    pub(crate) fn bin_status(&mut self, _: &Value) -> Result<Value> {
        let status = if let Some(w) = &self.warm_folder {
            scope::Status { bins: self.dirty_folder_bins(), settings: false, organization: w.saved_directory != self.project.modular_directory, unfiled: false }
        } else {
            scope::status(&self.folder()?.project, self.committed_state().0)
        };
        let mut result = json!({"dirtyBins":status.bins,"projectSettingsDirty":status.settings,"unfiledItemsDirty":status.unfiled});
        if status.organization {
            result["projectDirectoryDirty"] = true.into();
        }
        Ok(result)
    }

    /// History restores model snapshots while the offline inventory and decoders live in the session.
    pub(crate) fn refresh_restored_media(&mut self, previous: &Project) {
        if self.folder_revision().is_none() {
            return;
        }
        let mut changed = false;
        for (&id, item) in &previous.items {
            let Some(before) = item.as_media() else { continue };
            if self.project.item(id).and_then(|it| it.as_media()).is_none_or(|after| before.media != after.media || before.offline != after.offline) {
                self.media.remove(id);
                changed = true;
            }
        }
        if changed || self.project.items.iter().any(|(id, it)| it.as_media().is_some() && previous.item(*id).and_then(|it| it.as_media()).is_none()) {
            crate::relink::refresh(self);
        }
    }

    pub(crate) fn refresh_folder_dirty(&mut self) {
        if self.trim_play.dynamic.is_some() {
            return;
        }
        if self.warm_folder.as_ref().is_some_and(|w| w.saved_directory == self.project.modular_directory) && self.dirty_folder_bins().is_empty() {
            self.saved_revision = self.revision;
        }
        if let Some(folder) = &self.editing_folder {
            if self.project.next_id < folder.project.next_id {
                Arc::make_mut(&mut self.project).next_id = folder.project.next_id;
            }
            if !scope::status(&folder.project, &self.project).dirty() {
                self.saved_revision = self.revision;
            }
        }
    }

    pub(crate) fn save_bin(&mut self, p: &Value) -> Result<Value> {
        if self.warm_folder.is_some() {
            let id = bin_id(p, &self.project, &self.project)?;
            return self.save_warm(Some(id));
        }
        let folder = self.folder()?;
        let id = bin_id(p, &folder.project, &self.project)?;
        let status = scope::status(&folder.project, &self.project);
        let reviewed = self.reviewed_relink.filter(|id| folder.project.item(*id) != self.project.item(*id));
        if !status.bins.contains(&id) {
            folder
                .snapshot
                .while_current(|| {
                    checkpoint(self.persistence.as_ref(), status.dirty().then(|| self.project.clone()), self.revision, self.path.clone(), reviewed)
                })
                .map_err(|e| EngineError::Other(format!("no new folder revision was published: {e}")))?;
            if let Some(f) = &mut self.editing_folder {
                f.cleanup = None;
            }
            if !status.dirty() {
                self.saved_revision = self.revision;
            }
            return Ok(json!({"bin":id,"bytes":0,"folderRevision":self.folder()?.snapshot.revision()}));
        }
        let mut copies = Vec::new();
        let partial = Arc::new(scope::project_transfer(&folder.project, &self.project, id, true, &mut copies)?);
        let remaining = scope::status(&partial, &self.project).dirty();
        let mut view = folder.view.clone();
        view.open_sequences.retain(|id| partial.sequence(*id).is_some());
        view.active_sequence = view.active_sequence.filter(|id| partial.sequence(*id).is_some());
        view.sequences.retain(|id, _| partial.sequence(*id).is_some());
        view.playheads.retain(|id, _| partial.sequence(*id).is_some());
        if remaining && self.persistence.is_some() {
            folder
                .snapshot
                .while_current(|| checkpoint(self.persistence.as_ref(), Some(self.project.clone()), self.revision, self.path.clone(), reviewed))
                .map_err(|e| EngineError::Other(format!("bin was not saved: {e}")))?;
        }
        let folder = self.editing_folder.as_mut().ok_or_else(|| EngineError::Other("missing project folder".into()))?;
        let mut moves = EditingFolder::transfers(&folder.departures, &self.project, Some(id));
        moves.copies = copies;
        let saved = folder
            .snapshot
            .save_transfers(&folder.project, &folder.view, &partial, &view, moves)
            .map_err(|e| EngineError::Other(format!("bin was not saved: {e}")))?;
        for backup in &saved.move_backups {
            if let Some(capture) = folder.departures.get_mut(&(BinId(backup.source), ItemId(backup.sequence))) {
                capture.cut = CapturedCut::Archived(backup.snapshot.clone());
            }
        }
        folder.departures.retain(|(source, _), _| *source != id);
        if self.project.next_id < partial.next_id {
            Arc::make_mut(&mut self.project).next_id = partial.next_id;
        }
        folder.project = partial;
        folder.view = view;
        self.history.merge_key = None;
        if !remaining {
            self.saved_revision = self.revision;
        }
        if !remaining || reviewed.is_some_and(|id| folder.project.item(id) == self.project.item(id)) {
            folder.cleanup = Some(id);
            checkpoint(self.persistence.as_ref(), remaining.then(|| self.project.clone()), self.revision, self.path.clone(), None)
                .map_err(|e| EngineError::BinPublished { revision: saved.revision.clone(), message: e.to_string() })?;
        }
        folder.cleanup = None;
        self.report_retention_warning(saved.maintenance_warning.as_deref());
        Ok(json!({"bin":id,"bytes":saved.bytes,"retentionWarning":saved.maintenance_warning,"folderRevision":saved.revision}))
    }

    pub(crate) fn discard_bin(&mut self, p: &Value) -> Result<Value> {
        if self.warm_folder.is_some() {
            let id = bin_id(p, &self.project, &self.project)?;
            return self.discard_warm(id);
        }
        let folder = self.folder()?;
        let id = bin_id(p, &folder.project, &self.project)?;
        let status = scope::status(&folder.project, &self.project);
        let reviewed = self.reviewed_relink.filter(|id| folder.project.item(*id) != self.project.item(*id));
        if !status.bins.contains(&id) {
            folder
                .snapshot
                .while_current(|| {
                    checkpoint(self.persistence.as_ref(), status.dirty().then(|| self.project.clone()), self.revision, self.path.clone(), reviewed)
                })
                .map_err(|e| EngineError::Other(e.to_string()))?;
            return Ok(json!({"bin":id,"discarded":false,"folderRevision":folder.snapshot.revision()}));
        }
        let next = Arc::new(scope::project_bin(&self.project, &folder.project, id)?);
        let remaining = scope::status(&folder.project, &next).dirty();
        let undo = rebased_history(&self.history.undo, &folder.project, id, &next);
        let redo = rebased_history(&self.history.redo, &folder.project, id, &next);
        let rev = folder.snapshot.revision().to_owned();
        folder
            .snapshot
            .while_current(|| {
                checkpoint(
                    self.persistence.as_ref(),
                    remaining.then(|| next.clone()),
                    self.revision + 1,
                    self.path.clone(),
                    reviewed.filter(|id| next.item(*id) != folder.project.item(*id)),
                )
            })
            .map_err(|e| EngineError::Other(format!("bin was not discarded: {e}")))?;
        if let Some(folder) = &mut self.editing_folder {
            // Restoring the source bin cancels that outstanding departure. A
            // destination that is still open keeps its independently editable cut.
            folder.departures.retain(|(source, _), _| *source != id);
        }
        let previous = std::mem::replace(&mut self.project, next);
        self.refresh_restored_media(&previous);
        self.history.undo = undo;
        self.history.redo = redo;
        self.history.merge_key = None;
        self.fix_state();
        self.bump();
        if !remaining {
            self.saved_revision = self.revision;
        }
        Ok(json!({"bin":id,"discarded":true,"folderRevision":rev}))
    }
}

#[cfg(test)]
mod tests;
