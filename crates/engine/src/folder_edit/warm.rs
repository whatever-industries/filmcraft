//! Opt-in desktop session for a reviewed relink or a trim in one resident sequence. General edits and exports
//! first hydrate a complete project, including every reachable undo/redo state.
use super::EditingFolder;
use crate::{EngineError, Result, Session, autosave::ItemDraft};
use filmcraft_format::{
    Loaded,
    project_folder::{
        FolderSnapshot, Reader,
        index::{Index, warm_index},
        lock_source_revision, save_relinked_source, save_trimmed_sequence,
    },
};
use filmcraft_project::{BinEntry, BinId, FindQuery, ItemId, Project, ProjectItem, ProjectView};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

pub(crate) struct WarmFolder {
    reader: Reader,
    index: Index,
    manifest: PathBuf,
    pub revision: String,
    pub view: ProjectView,
    pub edited_item: Option<(BinId, ProjectItem)>,
    pub cleanup: Option<BinId>,
}

impl WarmFolder {
    pub fn open(path: &Path, cache: &Path) -> Result<(Self, Loaded)> {
        let manifest = std::path::absolute(path).map_err(error)?;
        let mut reader = Reader::open(&manifest).map_err(error)?;
        let index = warm_index(&manifest, cache, &reader).map_err(error)?;
        if reader.directory.retention_due().map_err(error)? {
            return Err(error("bin backup maintenance is due"));
        }
        let revision = reader.directory.revision.clone();
        let _guard = lock_source_revision(&manifest, &revision).map_err(error)?;
        reader.startup(false).map_err(error)?;
        let first = index.labels.iter().find_map(|(&id, label)| label.sequence.then_some(id));
        let playheads = reader.view.playheads.keys().copied().collect::<Vec<_>>();
        reader.load(index.labels.iter().filter_map(|(&id, label)| (!label.sequence).then_some(id)).chain(first).chain(playheads).collect()).map_err(error)?;
        let loaded = reader.take_loaded().map_err(error)?;
        if loaded.project.items.len() == index.labels.len() {
            return Err(error("all records are resident"));
        }
        let view = reader.view.clone();
        Ok((Self { reader, index, manifest, revision, view, edited_item: None, cleanup: None }, loaded))
    }

    pub fn sequence_name(&self, id: ItemId) -> Option<&str> {
        self.index.labels.get(&id).filter(|l| l.sequence).map(|l| l.name.as_str())
    }

    pub fn draft(&self, project: &Project, revision: u64) -> Option<ItemDraft> {
        let (_, saved) = self.edited_item.as_ref()?;
        let working = project.item(saved.id)?;
        (working != saved).then(|| ItemDraft {
            manifest: self.manifest.clone(),
            base_revision: self.revision.clone(),
            saved: saved.clone(),
            working: working.clone(),
            revision,
        })
    }

    pub fn lock(&self) -> Result<std::fs::File> {
        lock_source_revision(&self.manifest, &self.revision).map(|v| v.0).map_err(error)
    }
}

impl Session {
    /// Consume an existing verified index for desktop opens in this process.
    pub fn enable_experimental_modular_relink(&mut self, data_root: PathBuf) {
        self.warm_cache = Some(data_root);
    }
    pub fn has_deferred_timelines(&self) -> bool {
        self.warm_folder.is_some()
    }

    pub(crate) fn prepare_warm_command(&mut self, id: &str, p: &Value) -> Result<()> {
        let Some(w) = &self.warm_folder else { return Ok(()) };
        match id {
            "file.open"
            | "file.autoSaveStatus"
            | "file.recoveryList"
            | "file.autoSaveNow"
            | "edit.find"
            | "edit.findNext"
            | "media.linkMedia"
            | "media.search"
            | "media.status"
            | "media.findMissing"
            | "project.binStatus"
            | "project.saveBin"
            | "project.discardBin"
            | "edit.undo"
            | "edit.redo" => Ok(()),
            "timeline.select" | "playhead.set" | "playhead.step" => Ok(()),
            "sequence.open" => {
                let ids = p["item"].as_u64().map(ItemId).filter(|id| self.project.item(*id).is_none()).into_iter().collect();
                self.hydrate_visible(ids)
            }
            "timeline.trim" if p["mode"] != "ripple" => self.prepare_warm_trim(),
            "project.select"
                if p["items"].as_array().is_some_and(|ids| {
                    ids.iter().filter_map(Value::as_u64).all(|id| !w.index.labels.contains_key(&ItemId(id)) || self.project.item(ItemId(id)).is_some())
                }) =>
            {
                Ok(())
            }
            "media.relink" if (p.get("review").is_some() || p["reviewOnly"] == true) && w.edited_item.is_none() => Ok(()),
            "file.save"
                if p.get("path").is_none()
                    && (self.project_view() == w.view || w.edited_item.as_ref().is_none_or(|(_, item)| item.as_sequence().is_some())) =>
            {
                Ok(())
            }
            _ => self.promote_warm_folder(),
        }
    }

    fn prepare_warm_trim(&mut self) -> Result<()> {
        let Some(id) = self.state.active_sequence else { return Ok(()) };
        let Some(w) = &mut self.warm_folder else { return Ok(()) };
        let Some(item) = self.project.item(id) else { return self.promote_warm_folder() };
        if w.edited_item.as_ref().is_some_and(|(_, saved)| saved.id != id) || w.reader.directory.is_sequence_copy(id) {
            return self.promote_warm_folder();
        }
        let Some(owner) = crate::relink::review::owning_bin(&self.project, id) else { return self.promote_warm_folder() };
        let _guard = w.lock()?;
        if w.edited_item.is_none() {
            let mut remaining = w.reader.directory.remaining_item_bytes;
            let saved = w.reader.directory.read_item(id, &mut remaining).map_err(error)?;
            if &saved != item {
                drop(_guard);
                return self.promote_warm_folder();
            }
            w.edited_item = Some((owner, saved));
        }
        Ok(())
    }

    pub(crate) fn prepare_warm_find(&mut self, query: &FindQuery) -> Result<()> {
        let Some(w) = &self.warm_folder else { return Ok(()) };
        let Some(matches) = query.name_matcher() else { return self.promote_warm_folder() };
        let ids = w.index.labels.iter().filter_map(|(&id, label)| (self.project.item(id).is_none() && matches(&label.name)).then_some(id)).collect();
        self.hydrate_visible(ids)
    }

    pub fn prepare_bin_view(&mut self, id: BinId) -> Result<()> {
        if self.warm_folder.is_none() {
            return Ok(());
        }
        let ids = self
            .project
            .root
            .find_bin(id)
            .into_iter()
            .flat_map(|bin| &bin.children)
            .filter_map(|entry| match entry {
                BinEntry::Item(id) if self.project.item(*id).is_none() => Some(*id),
                _ => None,
            })
            .collect();
        self.hydrate_visible(ids)
    }

    pub fn prepare_closing_sequences(&mut self, ids: &[ItemId]) -> Result<()> {
        if self.warm_folder.is_some()
            && self.state.active_sequence.is_some_and(|id| ids.contains(&id))
            && let Some(&next) = self.state.open_sequences.iter().rev().find(|id| !ids.contains(id))
            && self.project.item(next).is_none()
        {
            self.hydrate_visible(vec![next])?;
        }
        Ok(())
    }

    fn hydrate_visible(&mut self, ids: Vec<ItemId>) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let Some(w) = &mut self.warm_folder else { return Ok(()) };
        if w.edited_item.as_ref().is_some_and(|(_, item)| item.as_media().is_some()) {
            return self.promote_warm_folder();
        }
        let _guard = w.lock()?;
        let added = w.reader.missing(ids, &self.project.items).map_err(error)?;
        if added.is_empty() {
            return Ok(());
        }
        let ids: Vec<_> = added.keys().copied().collect();
        self.install_warm_records(added)?;
        if !self.history.undo.is_empty() || !self.history.redo.is_empty() {
            let records: Vec<_> = ids
                .into_iter()
                .map(|id| self.project.item(id).cloned().map(|item| (id, item)).ok_or_else(|| error("missing loaded item")))
                .collect::<Result<_>>()?;
            for (_, project, _) in self.history.undo.iter_mut().chain(&mut self.history.redo) {
                Arc::make_mut(project).items.extend(records.iter().cloned());
            }
        }
        Ok(())
    }

    fn install_warm_records(&mut self, added: impl IntoIterator<Item = (ItemId, ProjectItem)>) -> Result<()> {
        let mut project = (*self.project).clone();
        project.items.extend(added);
        let loaded = Loaded { project, schema_version: self.loaded_schema, generator: None, view: None };
        self.project = Arc::new(filmcraft_format::validate_loaded(loaded).map_err(error)?.project);
        Ok(())
    }

    pub(crate) fn load_warm_impact(&mut self, id: ItemId) -> Result<()> {
        let Some(w) = &mut self.warm_folder else { return Ok(()) };
        let ids = w.index.impact_items(id).map_err(error)?;
        // Re-read the pinned impact set under Apply's revision lock, including
        // any records previously loaded for playback or discovery.
        let mut budget = w.reader.directory.remaining_item_bytes;
        let mut added = Vec::new();
        for item in ids {
            let verified = w.reader.directory.read_item(item, &mut budget).map_err(error)?;
            if let Some(resident) = self.project.item(item) {
                if resident != &verified {
                    return Err(error("source or dependent record changed; reopen for full review"));
                }
            } else {
                added.push((item, verified));
            }
        }
        if !added.is_empty() {
            self.install_warm_records(added)?;
        }
        Ok(())
    }

    pub(crate) fn promote_warm_folder(&mut self) -> Result<()> {
        let Some(w) = &self.warm_folder else { return Ok(()) };
        let _guard = w.lock()?;
        let (mut snapshot, loaded) = FolderSnapshot::open(&w.manifest).map_err(error)?;
        let mut base = loaded.project;
        crate::still_migration::migrate_legacy_still_sizes(&mut base, &self.media, &*self.services);
        let base = Arc::new(base);
        let merge = |partial: &Project| -> Result<Arc<Project>> {
            let Some((_, saved)) = &w.edited_item else { return Ok(base.clone()) };
            if base.item(saved.id) != Some(saved) {
                return Err(error("the saved item changed during promotion"));
            }
            let working = partial.item(saved.id).ok_or_else(|| error("missing edited item in history"))?;
            let mut result = base.clone();
            if working != saved {
                Arc::make_mut(&mut result).items.insert(saved.id, working.clone());
            }
            Ok(result)
        };
        let current = merge(&self.project)?;
        let rebase = |entries: &[super::Entry]| -> Result<Vec<super::Entry>> {
            entries.iter().map(|(label, project, origin)| Ok((label.clone(), merge(project)?, origin.clone()))).collect()
        };
        let undo = rebase(&self.history.undo)?;
        let redo = rebase(&self.history.redo)?;
        let warning = snapshot.schedule_retention().err();
        self.editing_folder = Some(EditingFolder { snapshot, project: base, view: w.view.clone(), departures: Default::default(), cleanup: w.cleanup });
        self.project = current;
        self.promoted_undo = self.history.undo.last().zip(undo.last()).map(|(old, new)| (Arc::downgrade(&old.1), Arc::downgrade(&new.1)));
        self.history.undo = undo;
        self.history.redo = redo;
        self.warm_folder = None;
        self.media.add_stored(&self.project, self.path.as_deref());
        crate::relink::refresh(self);
        if let Some(e) = warning {
            self.report_retention_warning(Some(&e.to_string()));
        }
        Ok(())
    }

    pub(crate) fn save_warm(&mut self, bin: Option<BinId>) -> Result<Value> {
        let started = web_time::Instant::now();
        let current_view = self.project_view();
        let w = self.warm_folder.as_mut().ok_or_else(|| error("expected modular project"))?;
        if bin.is_some_and(|id| self.project.root.find_bin(id).is_none()) {
            return Err(error("unknown bin"));
        }
        let mut bytes = 0;
        let mut warning = None;
        let mut resolved = true;
        let view = (bin.is_none() && current_view != w.view).then_some(&current_view);
        if let Some((owner, saved)) = &mut w.edited_item
            && let Some(working) = self.project.item(saved.id)
            && working != saved
        {
            if bin.is_none_or(|id| id == *owner) {
                let saved_change = if saved.as_sequence().is_some() {
                    save_trimmed_sequence(&w.manifest, &w.revision, Some((*owner, saved, working)), view)
                } else {
                    save_relinked_source(&w.manifest, &w.revision, *owner, saved, working)
                }
                .map_err(error)?;
                w.revision = saved_change.revision;
                bytes = saved_change.bytes_written;
                warning = saved_change.maintenance_warning;
                *saved = working.clone();
                w.cleanup = Some(*owner);
            } else {
                resolved = false;
            }
        }
        if let Some(view) = view {
            if bytes == 0 {
                let change = save_trimmed_sequence(&w.manifest, &w.revision, None, Some(view)).map_err(error)?;
                w.revision = change.revision;
                bytes = change.bytes_written;
                warning = change.maintenance_warning;
            }
            w.view = current_view;
        }
        let _guard = if bytes == 0 { Some(w.lock()?) } else { None };
        if resolved {
            self.saved_revision = self.revision;
            if let Some(worker) = &self.persistence {
                worker.clear_relink_checkpoint().map_err(|message| EngineError::BinPublished { revision: w.revision.clone(), message })?;
            }
            w.cleanup = None;
        }
        let mut result = json!({"bytes":bytes,"retentionWarning":warning,"folderRevision":w.revision});
        if let Some(id) = bin {
            result["bin"] = json!(id);
        } else {
            self.previews_follow_path();
            self.note_recent_project();
            result["path"] = json!(self.path);
            result["ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            result["backup"] = Value::Null;
        }
        self.report_retention_warning(warning.as_deref());
        Ok(result)
    }

    pub(crate) fn discard_warm(&mut self, id: BinId) -> Result<Value> {
        if self.project.root.find_bin(id).is_none() {
            return Err(error("unknown bin"));
        }
        let w = self.warm_folder.as_ref().ok_or_else(|| error("expected modular project"))?;
        let _guard = w.lock()?;
        let revision = w.revision.clone();
        let discarded = w.edited_item.as_ref().is_some_and(|(bin, item)| *bin == id && self.project.item(item.id) != Some(item));
        if discarded && let Some((_, saved)) = &w.edited_item {
            if let Some(p) = &self.persistence {
                p.clear_relink_checkpoint().map_err(error)?;
            }
            let old = self.project.clone();
            Arc::make_mut(&mut self.project).items.insert(saved.id, saved.clone());
            self.history.clear();
            self.refresh_restored_media(&old);
            self.bump();
            self.saved_revision = self.revision;
        }
        Ok(json!({"bin":id,"discarded":discarded,"folderRevision":revision}))
    }
}
