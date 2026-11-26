use super::*;
use filmcraft_format::project_folder::pack;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn fixture(dir: &Path) -> PathBuf {
    let source = dir.join("cut.fcproj");
    let p = filmcraft_format::decode(include_bytes!("../../../format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&p, false)).unwrap();
    doc["project"]["root"]["children"] = json!([
        {"Bin":{"id":70,"name":"A","children":[{"Item":21}]}},
        {"Bin":{"id":71,"name":"B","children":[]}},{"Item":5}]);
    doc["project"]["next_id"] = 2000.into();
    doc.pointer_mut("/project/items/21/kind/Sequence/video_tracks/0/items/0").unwrap()["extension"] = json!({"camera":"A"});
    fs::write(&source, serde_json::to_vec(&doc).unwrap()).unwrap();
    let dir = dir.join("cut");
    pack(&source, &dir, filmcraft_format::atomic_write_new).unwrap();
    dir.join(ROOT)
}
fn editor(dir: &Path) -> Session {
    Session::desktop_project_editor(AutosaveConfig::new(dir.join("user"))).unwrap()
}
fn open(s: &mut Session, path: &Path) {
    s.execute("file.open", json!({"path":path})).unwrap();
}
fn rename(s: &mut Session, bin: u64, name: &str) {
    s.execute("project.renameBin", json!({"bin":bin,"name":name})).unwrap();
}
fn name(s: &Session, b: u64) -> &str {
    &s.project.root.find_bin(BinId(b)).unwrap().name
}

#[test]
fn separate_histories_composite_export_and_release() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let folder = fixture(dir);
    let mut s = editor(dir);
    open(&mut s, &folder);
    rename(&mut s, 70, "Camera A");
    rename(&mut s, 71, "Camera B");
    let mut peer = editor(dir);
    open(&mut peer, &folder);
    assert_eq!(peer.open_desktop_bin(70).unwrap()["readOnly"], true);
    assert!(peer.execute("project.renameBin", json!({"bin":70,"name":"Lost"})).is_err());
    s.open_desktop_bin(70).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((name(&s, 70), name(&s, 71)), ("A", "Camera B"));
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("sequence.open", json!({"item":21})).unwrap();
    s.execute("timeline.delete", json!({"clip":29})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let copy = dir.join("export.fcproj");
    s.execute("file.saveCopy", json!({"path":copy})).unwrap();
    let bytes = fs::read(&copy).unwrap();
    let project = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(project.root.find_bin(BinId(70)).unwrap().name, "Camera A");
    assert_eq!(project.root.find_bin(BinId(71)).unwrap().name, "Camera B");
    let raw: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(raw.pointer("/project/items/21/kind/Sequence/video_tracks/0/items/0/extension/camera"), Some(&json!("A")));
    s.close_desktop_bin(70).unwrap();
    peer.close_desktop_bin(70).unwrap();
    assert_eq!(peer.open_desktop_bin(70).unwrap()["readOnly"], false);
    rename(&mut peer, 70, "Peer");
    peer.close_desktop_bin(70).unwrap();
    let composite = dir.join("latest.fcproj");
    s.execute("file.saveCopy", json!({"path":composite})).unwrap();
    let latest = filmcraft_format::decode(&fs::read(composite).unwrap()).unwrap().project;
    assert_eq!(latest.root.find_bin(BinId(70)).unwrap().name, "Peer");
    assert_eq!(latest.root.find_bin(BinId(71)).unwrap().name, "Camera B");
    s.execute("file.closeProject", json!({})).unwrap();
    open(&mut s, &folder);
    assert_eq!((name(&s, 70), name(&s, 71)), ("Peer", "Camera B"));
}

#[test]
fn crash_child() {
    let Some(dir) = std::env::var_os("FILMCRAFT_TEST_DESKTOP_BIN_CRASH").map(PathBuf::from) else { return };
    let mut s = editor(&dir);
    open(&mut s, &dir.join("cut").join(ROOT));
    for (bin, n) in [(70, "Recovered A"), (71, "Recovered B")] {
        rename(&mut s, bin, n);
        s.flush_folder_recovery().unwrap();
    }
    fs::write(dir.join("ready"), []).unwrap();
    std::io::stdin().read_line(&mut String::new()).unwrap();
}
#[test]
fn killed_writer_recovers_both_bins_independently() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    let path = fixture(dir);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "desktop_bins::tests::crash_child"])
        .env("FILMCRAFT_TEST_DESKTOP_BIN_CRASH", dir)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !dir.join("ready").exists() {
        assert!(child.try_wait().unwrap().is_none() && Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let mut s = editor(dir);
    let rows = s.execute("file.recoveryList", json!({})).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (bin, n) in [(70, "Recovered A"), (71, "Recovered B")] {
        let row = rows.iter().find(|r| r["bin"] == bin).unwrap();
        s.execute("file.recover", row.clone()).unwrap();
        assert_eq!(name(&s, bin), n);
    }
    let mut other = editor(dir);
    open(&mut other, &path);
    assert_eq!((name(&other, 70), name(&other, 71)), ("A", "B"));
    assert_eq!((name(&s, 70), name(&s, 71)), ("Recovered A", "Recovered B"));
    s.execute("file.closeProject", json!({})).unwrap();
    open(&mut s, &path);
    assert_eq!((name(&s, 70), name(&s, 71)), ("Recovered A", "Recovered B"));
}

#[test]
fn failed_second_publish_keeps_draft_and_both_leases_for_retry() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let manifest = fixture(dir);
    let ledger = manifest.with_file_name(".shared-next-id");
    let mut s = editor(dir);
    open(&mut s, &manifest);
    rename(&mut s, 70, "First");
    let first = fs::read(&ledger).unwrap();
    rename(&mut s, 71, "Second");
    let second = fs::read(&ledger).unwrap();
    fs::write(&ledger, &first).unwrap();
    let err = s.execute("file.open", json!({"path":dir.join("cut.fcproj")})).unwrap_err();
    assert!(err.to_string().contains("71"), "{err}");
    assert_eq!(s.path.as_deref(), manifest.to_str());
    assert_eq!(name(&s, 71), "Second");
    assert!(s.close_desktop_bin(71).is_err());
    let mut peer = editor(dir);
    open(&mut peer, &manifest);
    assert_eq!((name(&peer, 70), name(&peer, 71)), ("First", "B"));
    for bin in [70, 71] {
        assert_eq!(peer.open_desktop_bin(bin).unwrap()["readOnly"], true);
    }
    fs::write(&ledger, second).unwrap();
    assert_eq!(s.execute("file.saveAll", json!({})).unwrap()["savedBins"], json!([71]));
    s.execute("file.closeProject", json!({})).unwrap();
    open(&mut s, &manifest);
    assert_eq!((name(&s, 70), name(&s, 71)), ("First", "Second"));
}
