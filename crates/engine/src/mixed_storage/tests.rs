use super::*;
use filmcraft_format::project_folder::{self, ROOT, checkpoint::DESCRIPTOR};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const CHILD_DIR: &str = "FILMCRAFT_TEST_MIXED_WRITER_DIR";
const CLIP: &str = "/project/items/21/kind/Sequence/video_tracks/0/items/0";
const MEDIA: &str = "/project/items/5/kind/Media";

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let p = filmcraft_format::decode(include_bytes!("../../../format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&p, false)).unwrap();
    doc.pointer_mut(CLIP).unwrap()["vendor_extension"] = json!({"camera":"B","flags":[null,7]});
    let m = doc.pointer_mut(MEDIA).unwrap();
    m["media"] = json!({"File":{"path":"media/absent.mov"}});
    m["proxy"] = json!({"File":{"path":"proxies/absent.mov"}});
    let file = root.join("standard.fcproj");
    fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
    let folder = root.join("folder");
    project_folder::pack(&file, &folder, filmcraft_format::atomic_write_new).unwrap();
    (file, folder.join(ROOT))
}

fn editor(root: &Path) -> Session {
    let mut config = AutosaveConfig::new(root.join("data"));
    config.local_offset = |_| -7 * 3600;
    config.journal_debounce = Duration::from_secs(60);
    Session::local_project_editor(config).unwrap()
}

fn open(s: &mut Session, path: &Path) -> Value {
    s.execute("file.open", json!({"path":path})).unwrap()
}

fn rename(s: &mut Session, name: &str) {
    s.execute("project.renameBin", json!({"bin":1,"name":name})).unwrap();
}

fn bin(s: &Session) -> &str {
    &s.project.root.find_bin(filmcraft_project::BinId(1)).unwrap().name
}

fn copy(s: &mut Session, path: &Path) -> Value {
    s.execute("file.saveCopy", json!({"path":path})).unwrap();
    let bytes = fs::read(path).unwrap();
    filmcraft_format::decode(&bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// Executed in a real process so its file locks and workers disappear without Session::shutdown.
#[test]
fn crash_writer() {
    let Some(dir) = std::env::var_os(CHILD_DIR) else { return };
    let root = Path::new(&dir);
    let (file, folder) = fixture(root);
    let mut s = editor(root);
    open(&mut s, &file);
    rename(&mut s, "Saved standard");
    s.execute("file.save", json!({})).unwrap();
    if root.join("keep-standard-draft").is_file() {
        rename(&mut s, "Only in standard draft");
    }
    open(&mut s, &folder);
    assert_eq!(bin(&s), "Footage");
    let original = copy(&mut s, &root.join("before.fcproj"));
    s.execute("timeline.delete", json!({"clip":29})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    assert!(copy(&mut s, &root.join("deleted.fcproj")).pointer(CLIP).is_none());
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    open(&mut s, &folder);
    let restored = copy(&mut s, &root.join("restored.fcproj"));
    assert_eq!(restored.pointer(CLIP).unwrap(), original.pointer(CLIP).unwrap());
    for field in ["media", "proxy"] {
        assert_eq!(restored.pointer(MEDIA).unwrap()[field], original.pointer(MEDIA).unwrap()[field]);
    }
    assert_eq!(restored.pointer(CLIP).unwrap()["vendor_extension"], json!({"camera":"B","flags":[null,7]}));
    assert_eq!(restored.pointer(MEDIA).unwrap()["media"]["File"]["path"], "media/absent.mov");
    assert_eq!(restored.pointer(MEDIA).unwrap()["proxy"]["File"]["path"], "proxies/absent.mov");
    rename(&mut s, "Only in folder draft");
    let published = fs::read(&folder).unwrap();
    assert!(s.execute("file.autoSaveNow", json!({})).unwrap()["checkpointRevision"].is_number());
    assert_eq!(fs::read(&folder).unwrap(), published);
    fs::write(root.join("ready"), published).unwrap();
    io::stdin().read_line(&mut String::new()).unwrap();
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_writer(root: &Path) -> KillOnDrop {
    let mut child = KillOnDrop(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mixed_storage::tests::crash_writer", "--nocapture"])
            .env(CHILD_DIR, root)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !root.join("ready").is_file() {
        assert!(child.0.try_wait().unwrap().is_none(), "writer exited before checkpoint");
        assert!(Instant::now() < deadline, "writer did not checkpoint");
        std::thread::sleep(Duration::from_millis(10));
    }

    child
}

#[test]
fn killed_writer_recovers_both_storage_kinds_and_publishes_only_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let folder = root.join("folder").join(ROOT);
    let file = root.join("standard.fcproj");
    fs::write(root.join("keep-standard-draft"), []).unwrap();
    let mut child = start_writer(root);

    // The second session sees the published cut and can make an independent copy.
    let mut reader = editor(root);
    assert_eq!(open(&mut reader, &folder)["readOnly"], true);
    assert_eq!(bin(&reader), "Footage");
    assert!(reader.execute("file.save", json!({})).is_err());
    assert!(reader.execute("project.renameBin", json!({"bin":1,"name":"wrong"})).is_err());
    let exported = root.join("reader.fcproj");
    reader.execute("file.saveAs", json!({"path":exported})).unwrap();
    rename(&mut reader, "Independent copy");
    reader.execute("file.save", json!({})).unwrap();
    reader.shutdown();
    child.0.kill().unwrap();
    child.0.wait().unwrap();

    let mut session = editor(root);
    let listing = session.execute("file.recoveryList", json!({})).unwrap();
    let rows = listing.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let candidate = |kind: &str| rows.iter().find(|v| v["storage"] == kind).unwrap()["id"].clone();
    let std_id = candidate("projectFile");
    let folder_id = candidate("projectFolder");
    for cmd in ["file.recover", "file.discardRecovery"] {
        for args in [
            json!({}),
            json!({"id":std_id}),
            json!({"storage":"projectFolder"}),
            json!({"storage":"projectFile","id":false}),
            json!({"storage":"projectFolder","id":"missing"}),
        ] {
            assert!(session.execute(cmd, args).is_err(), "{cmd}");
        }
    }
    assert!(session.execute("file.discardRecovery", json!({"all":true})).is_err());
    let head = fs::read(root.join("ready")).unwrap();
    session.execute("file.recover", json!({"storage":"projectFolder","id":folder_id})).unwrap();
    assert_eq!(bin(&session), "Only in folder draft");
    assert!(session.is_dirty());
    assert_eq!(fs::read(&folder).unwrap(), head);
    session.execute("file.save", json!({})).unwrap();
    assert_ne!(fs::read(&folder).unwrap(), head);
    session.execute("file.recover", json!({"storage":"projectFile","id":std_id})).unwrap();
    assert_eq!(bin(&session), "Only in standard draft");
    session.execute("file.save", json!({})).unwrap();
    open(&mut session, &folder);
    assert_eq!(bin(&session), "Only in folder draft");
    open(&mut session, &file);
    assert_eq!(bin(&session), "Only in standard draft");
    assert_eq!(session.execute("file.recoveryList", json!({})).unwrap(), json!([]));
    session.shutdown();
}

#[test]
fn failed_switch_keeps_original_document_and_writer_in_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (file, folder) = fixture(root);
    let mut s = editor(root);
    open(&mut s, &file);
    rename(&mut s, "Standard work");
    let journal_path = |session: &mut Session| PathBuf::from(session.execute("file.autoSaveStatus", json!({})).unwrap()["sessionDir"].as_str().unwrap());
    let obstruction = journal_path(&mut s).join("snapshot.fcproj");
    fs::create_dir(&obstruction).unwrap();
    let original = s.project_instance.clone();
    assert!(s.execute("file.open", json!({"path":folder})).is_err());
    assert!(std::sync::Arc::ptr_eq(&original, &s.project_instance));
    assert!(s.is_dirty());
    assert_eq!(bin(&s), "Standard work");
    let mut peer = editor(root);
    assert_eq!(open(&mut peer, &folder)["readOnly"], false);
    peer.shutdown();
    fs::remove_dir(obstruction).unwrap();
    assert_eq!(open(&mut s, &folder)["readOnly"], false);
    rename(&mut s, "Folder work");
    let original = s.project_instance.clone();
    let broken = root.join("damaged.fcproj");
    fs::write(&broken, b"invalid project").unwrap();
    assert!(s.execute("file.open", json!({"path":broken})).is_err());
    let obstruction = journal_path(&mut s).join(DESCRIPTOR);
    fs::create_dir(&obstruction).unwrap();
    assert!(s.execute("file.open", json!({"path":file})).is_err());
    let output = root.join("new.fcproj");
    assert!(s.execute("file.saveAs", json!({"path":output})).is_err());
    assert!(!output.exists());
    assert!(std::sync::Arc::ptr_eq(&original, &s.project_instance));
    assert!(s.is_dirty());
    assert_eq!(bin(&s), "Folder work");
    let mut peer = editor(root);
    assert_eq!(open(&mut peer, &folder)["readOnly"], true);
    peer.shutdown();
    fs::remove_dir(obstruction).unwrap();
    open(&mut s, &file);
    assert_eq!(bin(&s), "Footage");
    let drafts = s.execute("file.recoveryList", json!({})).unwrap();
    assert_eq!(drafts.as_array().unwrap().len(), 2);
    for kind in ["projectFile", "projectFolder"] {
        let row = drafts.as_array().unwrap().iter().find(|v| v["storage"] == kind).unwrap();
        assert_eq!(s.execute("file.discardRecovery", row.clone()).unwrap()["discarded"], 1);
    }
    assert_eq!(s.execute("file.recoveryList", json!({})).unwrap(), json!([]));
    s.shutdown();
}

#[test]
fn desktop_preferences_and_late_standard_acknowledgement_are_document_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (file, folder) = fixture(root);
    let mut initial = editor(root);
    let mut prefs = initial.prefs.clone();
    prefs.auto_save.save_current_project = true;
    initial.set_prefs(prefs).unwrap();
    initial.shutdown();
    let mut s = editor(root);
    assert!(s.prefs.auto_save.save_current_project);
    open(&mut s, &file);
    rename(&mut s, "Autosaved file");
    // Wait for the disk write while leaving its acknowledgement unread by the session.
    s.persistence.as_ref().unwrap().auto_save_now().unwrap();
    open(&mut s, &folder);
    let head = fs::read(&folder).unwrap();
    rename(&mut s, "Private folder");
    s.poll_persistence();
    assert!(s.is_dirty());
    s.execute("file.autoSaveNow", json!({})).unwrap();
    assert_eq!(fs::read(&folder).unwrap(), head);
    open(&mut s, &file);
    assert_eq!(bin(&s), "Autosaved file");
    rename(&mut s, "Next edit");
    s.poll_persistence();
    assert!(s.is_dirty());
    s.execute("file.autoSaveNow", json!({})).unwrap();
    assert!(!s.is_dirty());
    open(&mut s, &file);
    assert_eq!(bin(&s), "Next edit");
    s.shutdown();
}

#[test]
fn folder_only_crash_can_export_the_draft_while_another_writer_owns_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    drop(start_writer(root));
    let folder = root.join("folder").join(ROOT);
    let head = fs::read(&folder).unwrap();
    let mut owner = editor(root);
    assert_eq!(open(&mut owner, &folder)["readOnly"], false);
    let mut recovering = editor(root);
    let rows = recovering.execute("file.recoveryList", json!({})).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["storage"], "projectFolder");
    let id = &rows[0]["id"];
    assert!(recovering.execute("file.recover", json!({"storage":"projectFolder","id":id})).is_err());
    let output = root.join("recovered.fcproj");
    recovering.execute("file.recover", json!({"storage":"projectFolder","id":id,"path":output})).unwrap();
    open(&mut recovering, &output);
    assert_eq!(bin(&recovering), "Only in folder draft");
    assert_eq!(bin(&owner), "Footage");
    assert_eq!(fs::read(&folder).unwrap(), head);
    assert_eq!(recovering.execute("file.recoveryList", json!({})).unwrap(), json!([]));
    owner.shutdown();
    recovering.shutdown();
}

#[test]
fn read_only_editor_can_find_saved_uses_while_the_writer_has_a_draft() {
    let dir = tempfile::tempdir().unwrap();
    let (_, folder) = fixture(dir.path());
    let mut writer = editor(dir.path());
    open(&mut writer, &folder);
    rename(&mut writer, "Private draft name");
    let mut reader = editor(dir.path());
    assert_eq!(open(&mut reader, &folder)["readOnly"], true);
    let history = reader.execute("project.binHistory", json!({})).unwrap();
    let revision = &history["revisions"][0][1];
    let report = reader.execute("project.findSavedItemUses", json!({"item":5,"revision":revision})).unwrap();
    assert_eq!(report["complete"], true);
    assert_eq!(report["revision"], *revision);
    assert_eq!(report["sequences"][0]["id"], 21);
    assert_eq!(report["sequences"][0]["direct"], true);
    assert!(report["filings"].as_array().unwrap().iter().any(|f| f["bin"]["name"] == "Footage"));
    assert!(!reader.is_dirty());
    assert!(reader.execute("file.save", json!({})).is_err());
    assert!(writer.is_dirty());
    assert_eq!(bin(&writer), "Private draft name");
    reader.shutdown();
    writer.shutdown();
}
