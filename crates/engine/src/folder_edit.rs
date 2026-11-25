//! Manual project-folder editing for a local headless session.
use crate::{EngineError, Result, Session};
use filmcraft_format::project_folder::{FolderSnapshot, ROOT};
use serde_json::{Value, json};
use std::path::Path;

pub(crate) struct EditingFolder {
    pub(crate) snapshot: FolderSnapshot,
    baseline: Vec<u8>,
}

pub(crate) fn manifest(path: &str) -> bool {
    Path::new(path).file_name().is_some_and(|name| name == ROOT)
}

fn typed(s: &Session) -> Vec<u8> {
    filmcraft_format::encode_with_view(&s.project, Some(&s.project_view()), false)
}

impl Session {
    /// Local filesystem session for the CLI. Restricted services, GUI and MCP sessions
    /// keep their normal single-document backend.
    pub fn local_folder_editor() -> Self {
        Self { allow_project_folders: true, ..Self::default() }
    }

    pub(crate) fn open_folder(&mut self, path: &str) -> Result<Value> {
        if !self.allow_project_folders || self.persistence.is_some() {
            return Err(EngineError::Other("project folders require a local manual editing session".into()));
        }
        let (mut snapshot, bytes) = FolderSnapshot::open(Path::new(path)).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
        let previous = self.editing_folder.take();
        let mut result = match crate::commands::open_bytes(self, path, &bytes) {
            Ok(result) => result,
            Err(error) => {
                self.editing_folder = previous;
                return Err(error);
            }
        };
        if let Some(mut old) = previous {
            snapshot.inherit_writer(&mut old.snapshot);
        }
        result["storage"] = json!("projectFolder");
        result["readOnly"] = json!(!snapshot.writable());
        result["folderRevision"] = json!(snapshot.revision());
        self.editing_folder = Some(EditingFolder { snapshot, baseline: typed(self) });
        Ok(result)
    }

    pub(crate) fn save_folder(&mut self, path: &str, adopt: bool) -> Result<Value> {
        let next = typed(self);
        let folder = self.editing_folder.as_mut().ok_or_else(|| EngineError::Other("open the folder before saving its manifest".into()))?;
        if manifest(path) {
            if !adopt || !folder.snapshot.is_manifest(Path::new(path)) {
                return Err(EngineError::Other("a folder can only publish to its open manifest".into()));
            }
            folder.snapshot.save(&folder.baseline, &next).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
            folder.baseline = next;
            self.saved_revision = self.revision;
            return Ok(json!({"path":path,"folderRevision":folder.snapshot.revision(),"readOnly":false}));
        }
        folder.snapshot.export(&folder.baseline, &next, Path::new(path)).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
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
