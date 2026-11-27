//! A private one-relink editor for disposable fixtures. Never pass its partial model to file.save.
use super::{Reader, Result, cache, discovery::Discovery};
use filmcraft_engine::{
    Session,
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
        Ok(Self { path, revision, candidate, view, saved: None, bytes_read, bytes_written: 0, decoded: 0, maintenance_warning: None })
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

    fn save(&mut self) -> Result<()> {
        let before = self.saved.as_ref().ok_or("apply a reviewed edit first")?;
        let after = self.source()?.clone();
        if before == &after {
            let (_guard, bytes) = lock_source_revision(&self.path, &self.revision)?;
            self.bytes_read += bytes;
            return Ok(());
        }
        let result = save_relinked_source(&self.path, &self.revision, self.candidate.bin, before, &after)?;
        self.decoded += 1;
        self.bytes_read += result.bytes_read;
        self.bytes_written += result.bytes_written;
        self.revision = result.revision;
        self.maintenance_warning = result.maintenance_warning;
        self.saved = Some(after);
        self.view.session.saved_revision = self.view.session.revision;
        Ok(())
    }

    fn discard(&mut self) -> Result<()> {
        let (_guard, bytes) = lock_source_revision(&self.path, &self.revision)?;
        self.bytes_read += bytes;
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
            "save" => self.save(),
            "discard" => self.discard(),
            "undo" | "redo" => {
                let s = &mut self.view.session;
                let label = if action == "undo" { s.undo() } else { s.redo() };
                label.ok_or("no relink history step")?;
                self.restored();
                Ok(())
            }
            _ => Err("expected apply, undo, redo, discard or save".into()),
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
pub(in super::super) fn baseline(path: &Path, input: &str) -> Result<()> {
    let review: Review = serde_json::from_str(input)?;
    let modular = path.extension().is_some_and(|ext| ext == "fcmod");
    let start = Instant::now();
    let mut editor = Session::local_folder_editor();
    let opened = editor.execute("file.open", json!({"path":path}))?;
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
    let began = Instant::now();
    let result = if modular { editor.execute("project.saveBin", json!({"bin":review.bin}))? } else { editor.execute("file.save", json!({}))? };
    let save = began.elapsed();
    println!(
        "{}",
        json!({"totalMs":start.elapsed().as_secs_f64()*1000.0,"applyMs":apply.as_secs_f64()*1000.0,"saveMs":save.as_secs_f64()*1000.0,
         "decoded":editor.project.items.len(),"save":result})
    );
    Ok(())
}

#[cfg(test)]
#[path = "source_edit_tests.rs"]
mod tests;
