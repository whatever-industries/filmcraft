//! A private one-relink editor for disposable fixtures. Never pass its partial model to file.save.
use super::{Reader, Result, cache, discovery::Discovery};
use filmcraft_engine::{
    Session,
    autosave::{AutoSavePrefs, AutosaveConfig, Persistence, RelinkDraft},
    relink::{
        self,
        review::{self, Review},
    },
};
use filmcraft_format::project_folder::{lock_source_revision, save_relinked_source};
use filmcraft_project::ProjectItem;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub(super) struct SourceEdit {
    path: PathBuf,
    data: PathBuf,
    journal: Option<Persistence>,
    revision: String,
    candidate: Review,
    view: Discovery,
    saved: Option<ProjectItem>,
    bytes_read: u64,
    bytes_written: u64,
    decoded: usize,
    maintenance_warning: Option<String>,
}
impl SourceEdit {
    pub(super) fn open(path: &Path, data: &Path, candidate: Review) -> Result<Self> {
        let path = std::path::absolute(path)?;
        let reader = Reader::open(&path)?;
        let revision = reader.directory.revision.clone();
        if revision != candidate.revision {
            return Err("stale project revision; refresh discovery".into());
        }
        let index = cache::warm_index(&path, data, &reader)?;
        let bytes_read = reader.bytes;
        let view = Discovery::new(&path, reader, index)?;
        Ok(Self {
            path,
            data: data.to_owned(),
            journal: None,
            revision,
            candidate,
            view,
            saved: None,
            bytes_read,
            bytes_written: 0,
            decoded: 0,
            maintenance_warning: None,
        })
    }

    fn source(&self) -> Result<&ProjectItem> {
        self.view.session.project.item(self.candidate.item).ok_or_else(|| "missing source record".into())
    }

    fn dirty(&self) -> bool {
        self.saved.as_ref().is_some_and(|saved| self.view.session.project.item(saved.id) != Some(saved))
    }

    fn apply(&mut self) -> Result<()> {
        if self.saved.is_some() {
            return Err("this prototype accepts one reviewed edit".into());
        }
        let (_guard, bytes) = lock_source_revision(&self.path, &self.candidate.revision)?;
        self.bytes_read += bytes;
        let ids = self.view.index.impact_items(self.candidate.item)?;
        let count = ids.len();
        let mut remaining = self.view.reader.directory.remaining_item_bytes;
        self.view.read_ids(ids, &mut remaining)?;
        self.bytes_read += self.view.reader.directory.remaining_item_bytes - remaining;
        self.decoded += count;
        let before = self.source()?.clone();
        let missing = relink::refresh(&mut self.view.session);
        if !missing.iter().any(|source| source.item == self.candidate.item.0 && source.status == "missing") {
            return Err("full editor required: the reviewed original is no longer a missing file".into());
        }
        review::apply_source_only(&mut self.view.session, &self.revision, &self.candidate)?;
        self.saved = Some(before);
        Ok(())
    }

    fn restored(&mut self) {
        self.view.session.media.remove(self.candidate.item);
        relink::refresh(&mut self.view.session);
        if !self.dirty() {
            self.view.session.saved_revision = self.view.session.revision;
        }
    }

    fn checkpoint(&mut self) -> Result<()> {
        if !self.dirty() {
            return self.clear_checkpoint();
        }
        let draft = RelinkDraft {
            manifest: self.path.clone(),
            base_revision: self.revision.clone(),
            saved: self.saved.clone().ok_or("missing source before-image")?,
            working: self.source()?.clone(),
            revision: self.view.session.revision,
        };
        if self.journal.is_none() {
            let prefs = AutoSavePrefs { enabled: false, ..Default::default() };
            self.journal = Some(Persistence::start(&AutosaveConfig::new(&self.data), prefs)?);
        }
        self.journal.as_ref().ok_or("no recovery worker")?.checkpoint_relink(draft)?;
        Ok(())
    }

    fn clear_checkpoint(&self) -> Result<()> {
        if let Some(worker) = &self.journal {
            worker.clear_relink_checkpoint()?;
        }
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        let before = self.saved.as_ref().ok_or("apply a reviewed edit first")?;
        let after = self.source()?.clone();
        if before == &after {
            let (_guard, bytes) = lock_source_revision(&self.path, &self.revision)?;
            self.bytes_read += bytes;
            return self.clear_checkpoint();
        }
        let result = save_relinked_source(&self.path, &self.revision, self.candidate.bin, before, &after)?;
        self.decoded += 1;
        self.bytes_read += result.bytes_read;
        self.bytes_written += result.bytes_written;
        self.revision = result.revision;
        self.maintenance_warning = result.maintenance_warning;
        self.saved = Some(after);
        self.view.session.saved_revision = self.view.session.revision;
        self.clear_checkpoint()
    }

    fn discard(&mut self) -> Result<()> {
        let (_guard, bytes) = lock_source_revision(&self.path, &self.revision)?;
        self.bytes_read += bytes;
        self.clear_checkpoint()?;
        if self.dirty() {
            let saved = self.saved.as_ref().ok_or("missing saved source")?;
            Arc::make_mut(&mut self.view.session.project).items.insert(saved.id, saved.clone());
            self.view.session.history.clear();
            self.restored();
        }
        Ok(())
    }

    fn step(&mut self, action: &str) -> Result<Value> {
        let began = Instant::now();
        self.action(action)?;
        Ok(json!({"action":action,"ms":began.elapsed().as_secs_f64()*1000.0,"unsaved":self.dirty(),"bytesWritten":self.bytes_written}))
    }

    fn action(&mut self, action: &str) -> Result<()> {
        match action {
            "apply" => self.apply(),
            "checkpoint" => self.checkpoint(),
            "save" => self.save(),
            "discard" => self.discard(),
            "undo" | "redo" => {
                let s = &mut self.view.session;
                let label = if action == "undo" { s.undo() } else { s.redo() };
                label.ok_or("no relink history step")?;
                self.restored();
                if self.journal.is_some() {
                    self.checkpoint()?;
                }
                Ok(())
            }
            _ => Err("expected apply, checkpoint, undo, redo, discard or save".into()),
        }
    }
}

pub(in super::super) fn run(path: &Path, input: &str, actions: &str) -> Result<()> {
    let review = serde_json::from_str(input)?;
    let data = filmcraft_engine::autosave::default_data_dir().ok_or("no FilmCraft data directory")?;
    let start = Instant::now();
    let mut edit = SourceEdit::open(path, &data, review)?;
    // Keep the working edit alive after a failed Save or Discard in a manual race test.
    if actions == "-" {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            let action = line.trim();
            let step = edit.step(action).unwrap_or_else(|error| json!({"action":action,"error":error.to_string(),"unsaved":edit.dirty()}));
            println!("{step}");
        }
        return Ok(());
    }
    let steps = actions.split(',').map(|action| edit.step(action)).collect::<Result<Vec<_>>>()?;
    println!(
        "{}",
        json!({"steps":steps,"totalMs":start.elapsed().as_secs_f64()*1000.0,"decoded":edit.decoded,"projectBytesRead":edit.bytes_read,
        "projectBytesWritten":edit.bytes_written,"revision":edit.revision,"maintenanceWarning":edit.maintenance_warning})
    );
    Ok(())
}

/// The normal editor performs an actual relink and Save on a disposable paired fixture.
pub(in super::super) fn baseline(path: &Path, input: &str, action: Option<&str>) -> Result<()> {
    let review: Review = serde_json::from_str(input)?;
    let modular = path.extension().is_some_and(|ext| ext == "fcmod");
    let start = Instant::now();
    let mut editor = Session::local_folder_editor();
    if let Some(action) = action {
        if !matches!(action, "save" | "checkpoint" | "checkpoint-pair" | "promote" | "full") {
            return Err("expected save, checkpoint, checkpoint-pair, promote or full".into());
        }
        let data = filmcraft_engine::autosave::default_data_dir().ok_or("no benchmark data root")?;
        if action != "full" {
            editor.enable_experimental_modular_relink(data.clone());
        }
        let mut config = AutosaveConfig::new(data);
        if action == "checkpoint-pair" {
            config.journal_debounce = std::time::Duration::from_secs(600);
        }
        editor.start_autosave(config)?;
        if action == "checkpoint-pair" {
            let mut prefs = editor.prefs.clone();
            prefs.auto_save.enabled = false;
            prefs.auto_save.recovery_interval_seconds = 600;
            editor.set_prefs(prefs)?;
        }
    }
    let opening = Instant::now();
    let opened = editor.execute("file.open", json!({"path":path}))?;
    let open_ms = opening.elapsed().as_secs_f64() * 1000.0;
    if action.is_some_and(|a| a != "full") && !editor.has_deferred_timelines() {
        return Err("expected a warm project with deferred timelines".into());
    }

    let mut args = json!({"item":review.item,"path":review.path,"relinkOthers":false,"alignTimecode":false});
    if modular {
        if opened["folderRevision"] != review.revision {
            return Err("stale baseline".into());
        }
        editor.execute("project.select", json!({"items":[review.item]}))?;
        args["bin"] = json!(review.bin);
        args["review"] = json!(review);
    }
    let began = Instant::now();
    editor.execute("media.relink", args)?;
    let apply = began.elapsed();
    let paired = action == Some("checkpoint-pair");
    if paired {
        use std::io::Write;
        println!("ready");
        std::io::stdout().flush()?;
        let mut command = String::new();
        std::io::stdin().read_line(&mut command)?;
        match command.trim() {
            "finish" => return Ok(()),
            "flush" => {}
            _ => return Err("expected flush or finish".into()),
        }
    }
    let began = Instant::now();
    if paired || action == Some("checkpoint") {
        let revision = editor.persistence.as_ref().ok_or("missing worker")?.flush()?;
        if revision != Some(editor.revision) {
            return Err("unconfirmed checkpoint".into());
        }
        editor.poll_persistence();
        let bytes = editor.persistence.as_ref().map(|p| p.status.last_bytes);
        println!(
            "{}",
            json!({"checkpointBytes":bytes,"openMs":open_ms,"checkpointMs":began.elapsed().as_secs_f64()*1000.0,"applyMs":apply.as_secs_f64()*1000.0})
        );
        if paired {
            use std::io::Write;
            std::io::stdout().flush()?;
            let mut end = String::new();
            std::io::stdin().read_line(&mut end)?;
            if end.trim() != "finish" {
                return Err("expected finish".into());
            }
        }
        return Ok(());
    }
    if action == Some("promote") {
        editor.execute("file.projectSettings.general", json!({}))?;
        if editor.has_deferred_timelines() || !editor.is_dirty() {
            return Err("expected an unsaved full editor".into());
        }
        println!("{}", json!({"openMs":open_ms,"promotionMs":began.elapsed().as_secs_f64()*1000.0,"applyMs":apply.as_secs_f64()*1000.0}));
        return Ok(());
    }
    let result = if modular { editor.execute("project.saveBin", json!({"bin":review.bin}))? } else { editor.execute("file.save", json!({}))? };
    let save = began.elapsed();
    println!(
        "{}",
        json!({"openMs":open_ms,"totalMs":start.elapsed().as_secs_f64()*1000.0,"applyMs":apply.as_secs_f64()*1000.0,"saveMs":save.as_secs_f64()*1000.0,
         "decoded":editor.project.items.len(),"save":result})
    );
    Ok(())
}

#[cfg(test)]
#[path = "source_edit_tests.rs"]
mod tests;
