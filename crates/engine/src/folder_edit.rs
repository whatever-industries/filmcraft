//! Manual project-folder editing for a local headless session.
use crate::{EngineError, Result, Session};
use filmcraft_format::project_folder::{EditOrigin, FolderSnapshot, ROOT};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

pub(crate) struct EditingFolder {
    pub(super) journal: Option<crate::folder_recovery::FolderJournal>,
    pub(crate) snapshot: FolderSnapshot,
    pub(super) baseline: Arc<FolderBase>,
}

impl EditingFolder {
    pub(super) fn scoped_project(&self, project: &Arc<filmcraft_project::Project>) -> Result<Arc<filmcraft_project::Project>> {
        if let Some(scope) = self.snapshot.scope() {
            let mut isolated = scope.project_onto(&self.baseline.project, project).map_err(|e| EngineError::Other(e.to_string()))?;
            isolated.next_id = self.snapshot.cursor();
            Ok(Arc::new(isolated))
        } else {
            Ok(project.clone())
        }
    }
}

/// Shared by all undo/redo entries made against this saved representation. The
/// immutable raw records stay in the folder. The project snapshot shares the engine's Arcs.
#[derive(Debug, PartialEq)]
pub(super) struct FolderBase {
    pub(super) origin: EditOrigin,
    pub(super) project: Arc<filmcraft_project::Project>,
    pub(super) view: filmcraft_project::ProjectView,
}

impl FolderBase {
    fn capture(s: &Session, revision: String) -> Arc<Self> {
        Arc::new(Self { origin: EditOrigin::Revision(revision), project: s.project.clone(), view: s.project_view() })
    }

    pub(super) fn typed(&self) -> Vec<u8> {
        filmcraft_format::encode_with_view(&self.project, Some(&self.view), false)
    }
}

pub(crate) struct Draft {
    pub(crate) source: filmcraft_format::project_folder::checkpoint::Source,
    pub(super) baseline: Arc<FolderBase>,
    pub(super) view: filmcraft_project::ProjectView,
}

impl Draft {
    pub(crate) fn encode(&self, project: &filmcraft_project::Project, directory: &Path) -> std::io::Result<Vec<u8>> {
        let folder = self.source.manifest.parent().ok_or_else(|| std::io::Error::other("missing source directory"))?;
        let mut isolated;
        let project = if let (Some(scope), Some(cursor)) = (&self.source.scope, self.source.cursor) {
            isolated = scope.project_onto(&self.baseline.project, project)?;
            isolated.next_id = cursor;
            &isolated
        } else {
            project
        };
        let after = filmcraft_format::encode_with_view(project, Some(&self.view), false);
        self.baseline.origin.checkpoint(folder, &self.baseline.typed(), &after, &directory.join(filmcraft_format::project_folder::checkpoint::DESCRIPTOR))
    }
}

pub(crate) fn manifest(path: &str) -> bool {
    Path::new(path).file_name().is_some_and(|name| name == ROOT)
}

impl Session {
    /// Local filesystem session for the CLI. Restricted services, GUI and MCP sessions
    /// keep their normal single-document backend.
    pub fn local_folder_editor() -> Self {
        Self { allow_project_folders: true, ..Self::default() }
    }

    pub(crate) fn open_folder(&mut self, path: &str) -> Result<Value> {
        if !self.allow_project_folders || (self.persistence.is_some() && self.mixed_config.is_none()) {
            return Err(EngineError::Other("project folders require a local manual editing session".into()));
        }
        if self.editing_bins() && self.editing_folder.as_ref().is_some_and(|f| f.snapshot.is_manifest(Path::new(path))) {
            return Ok(crate::commands::inspect_project(self));
        }
        let (mut snapshot, bytes) = (if self.shared_bins { FolderSnapshot::open_shared(Path::new(path)) } else { FolderSnapshot::open(Path::new(path)) })
            .map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let loaded = filmcraft_format::decode(&bytes).map_err(|e| EngineError::Other(e.to_string()))?;
        let can_write = snapshot.writable()
            || !snapshot.is_shared() && self.editing_folder.as_ref().is_some_and(|f| f.snapshot.is_manifest(Path::new(path)) && f.snapshot.writable());
        let journal = self
            .folder_recovery
            .as_ref()
            .filter(|_| can_write)
            .map(|d| crate::folder_recovery::FolderJournal::start(d, &snapshot, self.mixed_config.as_ref()))
            .transpose()?;
        self.prepare_document_switch()?;
        let previous = self.editing_folder.take();
        let mut result = crate::commands::install_loaded(self, path, loaded);
        if let Some(mut old) = previous {
            snapshot.inherit_writer(&mut old.snapshot);
        }
        result["storage"] = json!("projectFolder");
        result["readOnly"] = json!(!snapshot.writable());
        result["folderRevision"] = json!(snapshot.revision());
        let baseline = FolderBase::capture(self, snapshot.revision().to_owned());
        self.editing_folder = Some(EditingFolder { snapshot, baseline, journal });
        Ok(result)
    }

    pub(crate) fn save_folder(&mut self, path: &str, adopt: bool) -> Result<Value> {
        if self.editing_bins() && !manifest(path) {
            let result = self.export_bins(Path::new(path))?;
            if adopt {
                return crate::commands::open_project(self, path);
            }
            return Ok(result);
        }
        if adopt && !manifest(path) {
            self.flush_folder_recovery()?;
        }
        let view = self.project_view();

        let folder = self.editing_folder.as_mut().ok_or_else(|| EngineError::Other("open the folder before saving its manifest".into()))?;
        let scoped = folder.scoped_project(&self.project)?;
        let next = filmcraft_format::encode_with_view(&scoped, Some(&view), false);
        if manifest(path) {
            if !adopt || !folder.snapshot.is_manifest(Path::new(path)) {
                return Err(EngineError::Other("a folder can only publish to its open manifest".into()));
            }
            if let Some(journal) = &folder.journal {
                journal.verify_location(&folder.snapshot)?;
            }
            let (origin, published) = folder
                .snapshot
                .save_edit_with_origin(&folder.baseline.origin, &folder.baseline.typed(), &next)
                .map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            folder.baseline = Arc::new(FolderBase { origin, project: scoped, view });
            if let Some(published) = published {
                for (id, updated) in published.items.iter().filter(|(_, it)| !it.field_edits.is_empty()) {
                    if let Some(local) = Arc::make_mut(&mut self.project).item_mut(*id) {
                        local.merge_editorial_fields(updated, None);
                    }
                }
            }
            if let Some(j) = folder.journal.as_mut() {
                j.pending_cleanup = true;
            }
            self.saved_revision = self.revision;
            return Ok(json!({"path":path,"folderRevision":folder.snapshot.revision(),"readOnly":false}));
        }
        folder
            .snapshot
            .export_edit(&folder.baseline.origin, &folder.baseline.typed(), &next, Path::new(path))
            .map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        if adopt {
            // Reinstall the stock document so offline references use the exported location.
            return crate::commands::open_project(self, path);
        }
        Ok(json!({"path":path}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_format::project_folder::pack;
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    fn fixture(dir: &Path) -> PathBuf {
        let source = dir.join("opening.fcproj");
        let p = filmcraft_format::decode(include_bytes!("../../format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
        let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&p, false)).unwrap();
        doc["view"] = json!({"extra":"edit suite","open_sequences":[21],"active_sequence":21});
        doc["project"]["items"]["21"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["extension"] =
            json!({"timing":{"rate":[24,1]},"labels":[null,"camera"]});
        let effects = doc.pointer_mut("/project/items/21/kind/Sequence/video_tracks/0/items/0/effects").unwrap().as_array_mut().unwrap();
        let mut duplicate = effects[0].clone();
        duplicate["extra"] = json!("must keep");
        effects.push(duplicate);
        fs::write(&source, serde_json::to_vec(&doc).unwrap()).unwrap();
        let folder = dir.join("cut");
        pack(&source, &folder, filmcraft_format::atomic_write_new).unwrap();
        folder.join(ROOT)
    }

    #[test]
    fn failed_open_and_save_keep_draft_lock_and_revision_until_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = fixture(temp.path());
        let mut s = Session::local_folder_editor();
        let open = s.execute("file.open", json!({"path":path})).unwrap();
        assert!(s.start_autosave(crate::autosave::AutosaveConfig::new(temp.path().join("autosave"))).is_err());
        assert!(s.persistence.is_none());
        let saved = fs::read(&path).unwrap();
        let unchanged = s.execute("file.save", json!({})).unwrap();
        assert_eq!(unchanged["folderRevision"], open["folderRevision"]);
        assert_eq!(s.project.name, "Legacy Edit");
        s.execute("project.renameBin", json!({"bin":1,"name":"Camera A"})).unwrap();
        assert!(s.execute("file.open", json!({"path":temp.path().join("missing.fcproj")})).is_err());
        let mut reader = Session::local_folder_editor();
        assert_eq!(reader.execute("file.open", json!({"path":path})).unwrap()["readOnly"], true);
        let archive = path.with_file_name(open["folderRevision"].as_str().unwrap());
        fs::write(&archive, b"injected archive damage").unwrap();
        assert!(s.execute("file.save", json!({})).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert!(s.is_dirty());
        fs::remove_file(archive).unwrap();
        let next = s.execute("file.save", json!({})).unwrap();
        assert_ne!(open["folderRevision"], next["folderRevision"]);
        assert!(!s.is_dirty());
        assert_eq!(reader.execute("project.inspect", json!({})).unwrap()["root"]["children"][0]["name"], "Footage");
        assert_eq!(s.execute("file.revert", json!({})).unwrap()["readOnly"], false);
        s.execute("file.newProject", json!({})).unwrap();
        assert_eq!(reader.execute("file.revert", json!({})).unwrap()["readOnly"], false);
        assert_eq!(reader.project.root.children.len(), 2);
        assert_eq!(reader.execute("project.inspect", json!({})).unwrap()["root"]["children"][0]["name"], "Camera A");
    }

    #[test]
    fn ambiguous_clip_edit_keeps_draft_and_historical_data() {
        let temp = tempfile::tempdir().unwrap();
        let path = fixture(temp.path());
        let mut s = Session::local_folder_editor();
        let base = s.execute("file.open", json!({"path":path})).unwrap()["folderRevision"].as_str().unwrap().to_owned();
        let head = fs::read(&path).unwrap();
        let clip = &s.project.sequence(filmcraft_project::ItemId(21)).unwrap().video_tracks[0].items[0];
        let id = clip.id.0;
        let index = clip.effects.len() - 1;
        s.execute("effects.toggleEnabled", json!({"clip":id,"index":index})).unwrap();
        let error = s.execute("file.save", json!({})).unwrap_err().to_string();
        assert!(error.contains("item 21") && error.contains("effects"), "{error}");
        assert!(s.is_dirty());
        assert_eq!(fs::read(&path).unwrap(), head);
        let copy = temp.path().join("ambiguous.fcproj");
        assert!(s.execute("file.saveCopy", json!({"path":copy})).is_err());
        assert!(!copy.exists());
        s.execute("timeline.delete", json!({"clip":id})).unwrap();
        s.execute("file.save", json!({})).unwrap();
        let old = temp.path().join("before.fcproj");
        filmcraft_format::project_folder::recovery::rescue(path.parent().unwrap(), &base, 21, &old).unwrap();
        let old: Value = serde_json::from_slice(&fs::read(old).unwrap()).unwrap();
        let effects = old.pointer("/project/items/21/kind/Sequence/video_tracks/0/items/0/effects").unwrap();
        assert_eq!(effects.as_array().unwrap().last().unwrap()["extra"], "must keep");
    }

    #[test]
    fn undo_and_redo_after_folder_save_preserve_clip_extensions_and_revision_history() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let path = fixture(dir);
        let mut writer = Session::local_folder_editor();
        writer.execute("file.open", json!({"path":path})).unwrap();
        let export = |s: &mut Session, name: &str| -> Value {
            let out = dir.join(name);
            s.execute("file.saveCopy", json!({"path":out})).unwrap();
            let bytes = fs::read(out).unwrap();
            filmcraft_format::decode(&bytes).unwrap();
            serde_json::from_slice(&bytes).unwrap()
        };
        let reopen = |name: &str| -> Value {
            let mut reader = Session::local_folder_editor();
            assert_eq!(reader.execute("file.open", json!({"path":path})).unwrap()["readOnly"], true);
            export(&mut reader, name)
        };
        let original = export(&mut writer, "original.fcproj");
        let track = "/project/items/21/kind/Sequence/video_tracks/0";
        let before = original.pointer(track).unwrap();
        let clip = before["items"][0]["id"].as_u64().unwrap();
        assert!(before["items"][0]["extension"].is_object());
        writer.execute("timeline.delete", json!({"clip":clip})).unwrap();
        writer.execute("file.save", json!({})).unwrap();
        let deleted = reopen("deleted.fcproj");
        assert!(deleted.pointer(track).unwrap()["items"].as_array().unwrap().is_empty());
        writer.execute("edit.undo", json!({})).unwrap();
        assert_eq!(export(&mut writer, "undo-copy.fcproj").pointer(track), Some(before));
        let restored = writer.execute("file.save", json!({})).unwrap()["folderRevision"].as_str().unwrap().to_owned();
        assert_eq!(reopen("restored.fcproj").pointer(track), Some(before));
        writer.execute("edit.redo", json!({})).unwrap();
        writer.execute("file.save", json!({})).unwrap();
        assert_eq!(reopen("redone.fcproj")["project"], deleted["project"]);
        let past = dir.join("historical.fcproj");
        filmcraft_format::project_folder::recovery::rescue(path.parent().unwrap(), &restored, 21, &past).unwrap();
        let doc: Value = serde_json::from_slice(&fs::read(past).unwrap()).unwrap();
        assert_eq!(doc.pointer(track), Some(before));
        writer.execute("edit.undo", json!({})).unwrap();
        writer.execute("timeline.move", json!({"moves":[{"clip":clip,"track":"new","time":0}],"linked":false,"copy":true})).unwrap();
        assert!(!writer.history.can_redo());
        writer.execute("file.save", json!({})).unwrap();
        let branched = reopen("branched.fcproj");
        assert_eq!(branched.pointer(track), Some(before));
        let copy = branched.pointer("/project/items/21/kind/Sequence/video_tracks/1/items/0").unwrap();
        assert_ne!(copy["id"], clip);
        assert!(copy.get("extension").is_none());
    }

    #[test]
    fn missing_history_during_undo_save_leaves_the_draft_retryable() {
        let temp = tempfile::tempdir().unwrap();
        let path = fixture(temp.path());
        let mut writer = Session::local_folder_editor();
        let opened = writer.execute("file.open", json!({"path":path})).unwrap();
        let archive = path.with_file_name(opened["folderRevision"].as_str().unwrap());
        writer.execute("timeline.delete", json!({"clip":29})).unwrap();
        writer.execute("file.save", json!({})).unwrap();
        writer.execute("edit.undo", json!({})).unwrap();
        let head = fs::read(&path).unwrap();
        let held = temp.path().join("held-manifest");
        fs::rename(&archive, &held).unwrap();
        assert!(writer.execute("file.save", json!({})).is_err());
        assert!(writer.is_dirty() && writer.history.can_redo());
        assert_eq!(fs::read(path).unwrap(), head);
        fs::rename(held, archive).unwrap();
        writer.execute("file.save", json!({})).unwrap();
        assert!(!writer.is_dirty() && writer.history.can_redo());
    }

    #[test]
    fn dirty_switch_requires_a_checkpoint_and_isolates_same_named_projects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let data = root.join("data");
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let first = fixture(&a);
        let second = fixture(&b);
        let b_head = fs::read(&second).unwrap();
        let mut editor = Session::local_folder_editor();
        editor.enable_folder_recovery(&data).unwrap();
        editor.execute("file.open", json!({"path":first})).unwrap();
        let active = editor.execute("file.autoSaveStatus", json!({})).unwrap();
        let journal = PathBuf::from(active["sessionDir"].as_str().unwrap());
        let descriptor = journal.join(filmcraft_format::project_folder::checkpoint::DESCRIPTOR);
        // A failed descriptor publication leaves A open and its lock held.
        fs::create_dir(&descriptor).unwrap();
        editor.execute("project.renameBin", json!({"bin":1,"name":"Only in A"})).unwrap();
        assert!(editor.execute("file.open", json!({"path":second})).is_err());
        assert!(editor.is_dirty());
        let mut peer = Session::local_folder_editor();
        peer.enable_folder_recovery(&data).unwrap();
        assert_eq!(peer.execute("file.open", json!({"path":first})).unwrap()["readOnly"], true);
        assert!(peer.execute("file.recoveryList", json!({})).unwrap().as_array().unwrap().is_empty());
        fs::remove_dir(&descriptor).unwrap();
        editor.execute("file.open", json!({"path":second})).unwrap();
        assert!(!editor.is_dirty());
        let candidates = editor.execute("file.recoveryList", json!({})).unwrap();
        assert_eq!(candidates.as_array().unwrap().len(), 1);
        assert_eq!(Path::new(candidates[0]["projectPath"].as_str().unwrap()), first.canonicalize().unwrap());
        editor.execute("project.renameBin", json!({"bin":1,"name":"Only in B"})).unwrap();
        editor.execute("file.open", json!({"path":b.join("opening.fcproj")})).unwrap();
        assert_eq!(fs::read(second).unwrap(), b_head);
        let candidates = editor.execute("file.recoveryList", json!({})).unwrap();
        assert_eq!(candidates.as_array().unwrap().len(), 2);
        for (n, c) in candidates.as_array().unwrap().iter().enumerate() {
            let out = root.join(format!("{n}.fcproj"));
            editor.execute("file.recover", json!({"id":c["id"],"path":out})).unwrap();
            let p = filmcraft_format::decode(&fs::read(out).unwrap()).unwrap().project;
            let name = if Path::new(c["projectPath"].as_str().unwrap()) == first.canonicalize().unwrap() { "Only in A" } else { "Only in B" };
            assert_eq!(p.root.find_bin(filmcraft_project::BinId(1)).unwrap().name, name);
        }
    }

    #[test]
    fn worker_checkpoints_without_an_explicit_flush_and_preserves_edits_after_save() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(tmp.path());
        let data = tmp.path().join("data");
        let mut s = Session::local_folder_editor();
        s.enable_folder_recovery(&data).unwrap();
        s.execute("file.open", json!({"path":path})).unwrap();
        s.execute("project.renameBin", json!({"bin":1,"name":"Manual save"})).unwrap();
        s.execute("file.save", json!({})).unwrap();
        let manual = fs::read(&path).unwrap();
        s.execute("project.renameBin", json!({"bin":1,"name":"After manual save"})).unwrap();
        let expected = s.revision;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = s.execute("file.autoSaveStatus", json!({})).unwrap();
            if status["status"]["journaledRevision"] == expected {
                break;
            }
            assert!(Instant::now() < deadline, "{status}");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(s.is_dirty());
        assert_eq!(fs::read(&path).unwrap(), manual);
        drop(s);
        let mut fresh = Session::local_folder_editor();
        fresh.enable_folder_recovery(&data).unwrap();
        fresh.execute("file.recover", json!({})).unwrap();
        assert_eq!(fresh.project.root.find_bin(filmcraft_project::BinId(1)).unwrap().name, "After manual save");
        assert!(fresh.is_dirty());
        assert_eq!(fs::read(&path).unwrap(), manual);
    }

    #[test]
    fn busy_changed_and_missing_folders_allow_independent_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let path = fixture(root);
        let data = root.join("data");
        let mut s = Session::local_folder_editor();
        s.enable_folder_recovery(&data).unwrap();
        s.execute("file.open", json!({"path":path})).unwrap();
        s.execute("project.renameBin", json!({"bin":1,"name":"Unsaved name"})).unwrap();
        s.execute("file.open", json!({"path":root.join("opening.fcproj")})).unwrap();
        let candidate = s.execute("file.recoveryList", json!({})).unwrap()[0]["id"].clone();
        let mut other = Session::local_folder_editor();
        other.execute("file.open", json!({"path":path})).unwrap();
        assert!(s.execute("file.recover", json!({"id":candidate})).is_err());
        other.execute("project.renameBin", json!({"bin":1,"name":"New head"})).unwrap();
        other.execute("file.save", json!({})).unwrap();
        drop(other);
        let new_head = fs::read(&path).unwrap();
        assert!(s.execute("file.recover", json!({"id":candidate})).is_err());
        assert_eq!(fs::read(&path).unwrap(), new_head);
        fs::rename(path.parent().unwrap(), root.join("moved")).unwrap();
        assert!(s.execute("file.recover", json!({"id":candidate})).is_err());
        let dest = root.join("copy.fcproj");
        fs::write(&dest, b"occupied").unwrap();
        assert!(s.execute("file.recover", json!({"id":candidate,"path":dest})).is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"occupied");
        fs::remove_file(&dest).unwrap();
        s.execute("file.recover", json!({"id":candidate,"path":dest})).unwrap();
        assert_eq!(
            filmcraft_format::decode(&fs::read(dest).unwrap()).unwrap().project.root.find_bin(filmcraft_project::BinId(1)).unwrap().name,
            "Unsaved name"
        );
        assert!(s.execute("file.recoveryList", json!({})).unwrap().as_array().unwrap().is_empty());
    }

    #[test]
    fn writer_process() {
        let Some(path) = std::env::var_os("FILMCRAFT_TEST_FOLDER_WRITER") else {
            return;
        };
        let path = PathBuf::from(path);
        let mut writer = Session::local_folder_editor();
        assert_eq!(writer.execute("file.open", json!({"path":path})).unwrap()["readOnly"], false);
        fs::write(path.with_file_name("child-ready"), b"ready").unwrap();
        let mut release = String::new();
        std::io::stdin().read_line(&mut release).unwrap();
        drop(writer);
    }

    #[test]
    fn a_second_process_can_export_and_acquires_the_lock_after_writer_termination() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path());
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "folder_edit::tests::writer_process"])
            .env("FILMCRAFT_TEST_FOLDER_WRITER", &path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !path.with_file_name("child-ready").exists() {
            assert!(child.try_wait().unwrap().is_none() && Instant::now() < deadline, "writer did not open");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut second = Session::local_folder_editor();
        assert_eq!(second.execute("file.open", json!({"path":path})).unwrap()["readOnly"], true);
        assert!(second.execute("project.renameBin", json!({"bin":1,"name":"Oops"})).is_err());
        assert!(second.execute("file.save", json!({})).is_err());
        let dst = dir.path().join("copy.fcproj");
        second.execute("file.saveCopy", json!({"path":dst})).unwrap();
        assert!(filmcraft_format::decode(&fs::read(&dst).unwrap()).is_ok());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(second.execute("file.revert", json!({})).unwrap()["readOnly"], false);
        second.execute("file.saveAs", json!({"path":dir.path().join("independent.fcproj")})).unwrap();
        let mut third = Session::local_folder_editor();
        assert_eq!(third.execute("file.open", json!({"path":path})).unwrap()["readOnly"], false);
    }
}
