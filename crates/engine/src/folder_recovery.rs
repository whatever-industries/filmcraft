//! Opt-in recovery for local folder editors. Each open document has its own worker,
//! journal directory and lock. Its queue cannot act on any subsequent document.
use crate::{
    EngineError, Result, Session,
    autosave::{self, AutosaveConfig, Persistence},
    folder_edit::{EditingFolder, FolderBase},
};
use filmcraft_format::project_folder::{
    EditOrigin, FolderSnapshot,
    checkpoint::{Checkpoint, DESCRIPTOR, Source},
};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("folder recovery: {e}"))
}

pub(crate) struct FolderJournal {
    worker: Option<Persistence>,
    source: Source,
    last_sent: Option<(u64, bool)>,
}

impl FolderJournal {
    pub(crate) fn start(data: &Path, snapshot: &FolderSnapshot) -> Result<Self> {
        let source = Source::capture(snapshot.directory(), snapshot.revision()).map_err(error)?;
        if data.starts_with(snapshot.directory()) {
            return Err(error("choose a data directory outside the project folder"));
        }
        let worker = Persistence::start_folder(&AutosaveConfig::new(data)).map_err(error)?;
        Ok(Self { worker: Some(worker), source, last_sent: None })
    }

    pub(super) fn clear(&self) -> Result<()> {
        if let Some(w) = &self.worker {
            w.send_clean();
            w.flush().map_err(error)?;
        }
        Ok(())
    }

    pub(super) fn verify_location(&self, folder: &FolderSnapshot) -> Result<()> {
        let current = Source::capture(folder.directory(), &self.source.head).map_err(error)?;
        if current != self.source {
            return Err(error("the open project folder has been replaced"));
        }
        Ok(())
    }
}

impl Drop for FolderJournal {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.close_folder();
        }
    }
}

/// Hold this lock throughout verification, installation and retirement. Two recovering
/// processes must not consume one another's candidate.
struct Candidate {
    dir: PathBuf,
    checkpoint: Checkpoint,
    _lock: File,
}

fn claim(dir: &Path) -> io::Result<Candidate> {
    if !dir.symlink_metadata()?.is_dir() || !dir.join("lock").symlink_metadata()?.is_file() {
        return Err(io::Error::other("invalid recovery session directory or lock"));
    }
    let lock = File::options().read(true).write(true).open(dir.join("lock"))?;
    lock.try_lock().map_err(io::Error::from)?;
    Ok(Candidate { dir: dir.into(), checkpoint: Checkpoint::read(dir)?, _lock: lock })
}

impl Candidate {
    fn retire(self) -> Result<()> {
        Checkpoint::clear(&self.dir).map_err(error)?;
        let dir = self.dir.clone();
        drop(self);
        let _ = fs::remove_file(dir.join("lock"));
        let _ = fs::remove_dir(dir);
        Ok(())
    }
}

impl Session {
    pub fn enable_folder_recovery(&mut self, data: &Path) -> Result<()> {
        if !self.allow_project_folders || self.editing_folder.is_some() || self.persistence.is_some() {
            return Err(error("enable recovery in a fresh local folder editing session"));
        }
        let dir = data.join("FolderDrafts");
        fs::create_dir_all(&dir).map_err(error)?;
        self.folder_recovery = Some(dir.canonicalize().map_err(error)?);
        Ok(())
    }

    pub(super) fn sync_folder_recovery(&mut self) {
        let state = (self.revision, self.is_dirty());
        let Some(folder) = self.editing_folder.as_ref() else { return };
        let Some(journal) = &folder.journal else { return };
        if journal.last_sent == Some(state) {
            return;
        }
        let draft = state.1.then(|| {
            let mut source = journal.source.clone();
            source.head = folder.snapshot.revision().into();
            source.scope = folder.snapshot.scope().cloned();
            source.cursor = source.scope.as_ref().map(|_| folder.snapshot.cursor());
            crate::folder_edit::Draft { source, baseline: folder.baseline.clone(), view: self.project_view() }
        });
        if let Some(journal) = self.editing_folder.as_mut().and_then(|f| f.journal.as_mut())
            && let Some(worker) = &mut journal.worker
        {
            worker.drain_events();
            if let Some(draft) = draft {
                worker.send_folder(self.project.clone(), self.revision, draft);
            } else {
                worker.send_clean();
            }
            journal.last_sent = Some(state);
        }
    }

    pub fn flush_folder_recovery(&mut self) -> Result<Option<u64>> {
        self.sync_folder_recovery();
        let Some(worker) = self.editing_folder.as_ref().and_then(|f| f.journal.as_ref()).and_then(|j| j.worker.as_ref()) else { return Ok(None) };
        let written = worker.flush().map_err(error)?;
        if self.is_dirty() && written != Some(self.revision) {
            return Err(error("checkpoint generation did not match the open draft"));
        }
        Ok(written)
    }

    pub(super) fn folder_status(&mut self) -> Value {
        let Some(worker) = self.editing_folder.as_mut().and_then(|f| f.journal.as_mut()).and_then(|j| j.worker.as_mut()) else {
            return json!({"running":false});
        };
        worker.drain_events();
        json!({"running":worker.is_running(),"status":worker.status,"sessionDir":worker.session_dir})
    }

    pub(super) fn folder_candidates(&self) -> Result<Value> {
        let root = autosave::recovery_root(self.folder_recovery.as_ref().ok_or_else(|| error("disabled"))?);
        let mut entries = Vec::new();
        let dirs = match fs::read_dir(root) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(json!([])),
            Err(e) => return Err(error(e)),
        };
        for entry in dirs {
            let dir = entry.map_err(error)?.path();
            if !dir.symlink_metadata().map_err(error)?.is_dir() || !dir.join(DESCRIPTOR).try_exists().map_err(error)? {
                continue;
            }
            let id = dir.file_name().and_then(|s| s.to_str());
            let saved = dir
                .join(DESCRIPTOR)
                .symlink_metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis());
            match claim(&dir) {
                Ok(c) => entries.push(
                    json!({"id":id,"savedUnixMs":saved,"projectPath":c.checkpoint.source.manifest,"projectName":c.checkpoint.project_name,"revision":c.checkpoint.generation}),
                ),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => entries.push(json!({"id":id,"error":e.to_string()})),
            }
        }
        entries.sort_by(|a, b| b["savedUnixMs"].as_u64().cmp(&a["savedUnixMs"].as_u64()).then_with(|| b["id"].as_str().cmp(&a["id"].as_str())));
        Ok(json!(entries))
    }

    fn candidate(&self, params: &Value) -> Result<Candidate> {
        let id = match params.get("id") {
            Some(v) => v
                .as_str()
                .filter(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
                .ok_or_else(|| error("invalid recovery session id"))?
                .to_owned(),
            None => self.folder_candidates()?[0]["id"].as_str().ok_or_else(|| error("no recovery candidate"))?.to_owned(),
        };
        let root = autosave::recovery_root(self.folder_recovery.as_ref().ok_or_else(|| error("disabled"))?);
        claim(&root.join(id)).map_err(error)
    }

    pub(super) fn discard_folder_candidate(&self, params: &Value) -> Result<Value> {
        let mut count = 0;
        if params["all"] == true {
            for entry in self.folder_candidates()?.as_array().into_iter().flatten() {
                self.candidate(entry)?.retire()?;
                count += 1;
            }
        } else {
            self.candidate(params)?.retire()?;
            count = 1;
        }
        Ok(json!({"discarded":count}))
    }

    pub(super) fn recover_folder_candidate(&mut self, params: &Value) -> Result<Value> {
        let candidate = self.candidate(params)?;
        let c = &candidate.checkpoint;
        if let Some(dest) = params.get("path") {
            let dest = Path::new(dest.as_str().ok_or_else(|| error("expected a new .fcproj path"))?);
            if dest.extension().is_none_or(|x| x != "fcproj") {
                return Err(error("expected a new .fcproj path"));
            }
            let parent =
                std::path::absolute(dest).map_err(error)?.parent().ok_or_else(|| error("missing destination parent"))?.canonicalize().map_err(error)?;
            if self.folder_recovery.as_ref().is_some_and(|d| parent.starts_with(d)) || c.source.manifest.parent().is_some_and(|p| parent.starts_with(p)) {
                return Err(error("choose a destination outside the source and recovery directories"));
            }
            let bytes = c.document_at(&candidate.dir, dest).map_err(error)?;
            filmcraft_format::atomic_write_new(dest, &bytes).map_err(error)?;
            candidate.retire()?;
            return Ok(json!({"path":dest,"storage":"projectFile"}));
        }
        let path = c.source.manifest.clone();
        let independent = "recover to a new .fcproj with file.recover {id,path}";
        // Check location and digest before opening a potentially different project.
        let directory = path.parent().ok_or_else(|| error("missing project folder"))?.canonicalize().map_err(|e| error(format!("{e}; {independent}")))?;
        let identity = Source::capture(&directory, &c.source.head).map_err(error)?;
        if !identity.matches(&c.source) {
            return Err(error(independent));
        }
        let draft = c.document_at(&candidate.dir, &path).map_err(error)?;
        let (mut snapshot, bytes) = if let Some(scope) = &c.source.scope {
            if !self.shared_bins {
                return Err(error(format!("use --shared-bins or {independent}")));
            }
            let (mut snapshot, _) = FolderSnapshot::open_shared(&path).map_err(error)?;
            let cursor = c.source.cursor.ok_or_else(|| error(independent))?;
            let bytes = snapshot.checkout_bin(scope.bin, Some((&c.source.head, scope, &draft, cursor))).map_err(|e| error(format!("{e}; {independent}")))?;
            (snapshot, bytes)
        } else {
            if self.shared_bins {
                return Err(error(format!("use whole-project recovery or {independent}")));
            }
            let (snapshot, _) = FolderSnapshot::open(&path).map_err(|e| error(format!("{e}; {independent}")))?;
            let can_transfer = self.editing_folder.as_ref().is_some_and(|f| f.snapshot.is_manifest(&path) && f.snapshot.writable());
            if (!snapshot.writable() && !can_transfer) || snapshot.revision() != c.source.head {
                return Err(error(independent));
            }
            (snapshot, draft)
        };
        self.flush_folder_recovery()?;
        let journal = FolderJournal::start(self.folder_recovery.as_ref().ok_or_else(|| error("disabled"))?, &snapshot)?;
        if !Source::capture(snapshot.directory(), &c.source.head).map_err(error)?.matches(&c.source) {
            return Err(error(independent));
        }
        let path_text = path.to_str().ok_or_else(|| error("non-UTF-8 project path"))?;
        let previous = self.editing_folder.take();
        if let Err(e) = crate::commands::open_bytes(self, path_text, &bytes) {
            self.editing_folder = previous;
            return Err(e);
        }
        if let Some(mut old) = previous {
            snapshot.inherit_writer(&mut old.snapshot);
        }
        if snapshot.scope().is_some() {
            Arc::make_mut(&mut self.project).next_id = snapshot.cursor();
        }
        let baseline = Arc::new(FolderBase { origin: EditOrigin::Recovered(bytes.into()), project: self.project.clone(), view: self.project_view() });
        self.editing_folder = Some(EditingFolder { snapshot, baseline, journal: Some(journal) });
        self.saved_revision = 0;
        self.flush_folder_recovery()?;
        candidate.retire()?;
        Ok(json!({"path":path,"storage":"projectFolder","dirty":true}))
    }
}
