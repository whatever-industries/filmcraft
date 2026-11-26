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

/// Both edits must still decode the same pictures after their shared source loses its filing.
#[test]
fn removing_source_filing_preserves_two_cuts_on_cold_reopen() {
    use crate::media_test_util::{make_movie, session_with};
    use filmcraft_media::DemoScene;
    use filmcraft_project::TrackKind;
    use filmcraft_time::{FrameRate, Tick, TimeRange};

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let original = dir.join("interview.mov");
    let proxy = dir.join("interview-proxy.mov");
    make_movie(&original, DemoScene::Aurora, 160, 96, 12);
    make_movie(&proxy, DemoScene::Plasma, 80, 48, 12);
    let (mut setup, items, first) = session_with(&[&original]);
    let source = items[0];
    let source_name = setup.project.item(source).unwrap().name.clone();
    setup.execute("media.attachProxies", json!({"item":source.0,"path":proxy})).unwrap();
    let p = Arc::make_mut(&mut setup.project);
    let sources = p.add_bin("Interviews", None);
    let cuts = p.add_bin("Cuts", None);
    p.move_to_bin(&[source], Some(sources)).unwrap();
    p.move_to_bin(&[first], Some(cuts)).unwrap();
    let rate = FrameRate::FPS_24;
    let second = p.new_sequence("Short cut", p.sequence(first).unwrap().settings.clone(), 1, 1, Some(cuts));
    let clip = p.make_track_item(source, TrackKind::Video, Tick::ZERO, TimeRange::new(rate.tick_of(4), rate.tick_of(7)), rate).unwrap();
    let audio = p.make_track_item(source, TrackKind::Audio, Tick::ZERO, TimeRange::new(rate.tick_of(4), rate.tick_of(7)), rate).unwrap();
    p.sequence_mut(second).unwrap().video_tracks[0].items.push(clip);
    p.sequence_mut(second).unwrap().audio_tracks[0].items.push(audio);
    let stock = dir.join("edits.fcproj");
    setup.execute("file.saveAs", json!({"path":stock})).unwrap();
    let render = |s: &mut Session| {
        let mut frames = Vec::new();
        for id in [first, second] {
            s.execute("sequence.open", json!({"item":id.0})).unwrap();
            for frame in [0, 2] {
                let (image, offline) = s.try_render_program_with_offline_at(1.0, rate.tick_of(frame)).unwrap();
                assert!(offline.is_empty());
                let provider = s.media.provider(s.project.clone(), s.services.clone());
                let audio = filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), frame * 2000, 1000, &provider);
                assert!(audio.channels.iter().flatten().any(|v| v.abs() > 0.001));
                frames.push((image.over_black_rgba8(), audio.channels));
            }
        }
        frames
    };
    let expected = render(&mut setup);
    assert!(expected[0].0 != expected[2].0);
    setup.execute("media.toggleProxies", json!({"enabled":true})).unwrap();
    let expected_proxy = render(&mut setup);
    assert!(expected[0].0 != expected_proxy[0].0);
    drop(setup);
    let folder = dir.join("modular");
    pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let manifest = folder.join(ROOT);
    {
        let mut s = editor(dir);
        open(&mut s, &manifest);
        s.execute("project.removeFromBin", json!({"bin":sources.0,"items":[source.0]})).unwrap();
        let mut peer = editor(&dir.join("peer"));
        open(&mut peer, &manifest);
        assert!(peer.execute("project.removeFromBin", json!({"bin":sources.0,"items":[source.0]})).is_err());
        assert_eq!(peer.project.root.parent_of(source), Some(sources));
        s.execute("file.save", json!({})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.root.parent_of(source), Some(sources));
        s.execute("file.save", json!({})).unwrap();
        s.execute("edit.redo", json!({})).unwrap();
        s.execute("file.save", json!({})).unwrap();
    }
    // The exclusive folder writer can unfile the entire hierarchy in one edit.
    let second_folder = dir.join("whole-bin");
    pack(&stock, &second_folder, filmcraft_format::atomic_write_new).unwrap();
    let whole = second_folder.join(ROOT);
    {
        let mut s = Session::local_folder_editor();
        open(&mut s, &whole);
        s.execute("project.removeFromBin", json!({"bin":s.project.root.id.0,"bins":[sources.0]})).unwrap();
        s.execute("file.save", json!({})).unwrap();
    }
    {
        let mut s = editor(dir);
        open(&mut s, &whole);
        assert!(s.project.root.find_bin(sources).is_none());
        assert!(render(&mut s) == expected);
    }
    for (use_proxy, pictures) in [(false, expected), (true, expected_proxy)] {
        if use_proxy {
            fs::rename(&original, dir.join("disconnected.mov")).unwrap();
        }
        let mut s = editor(dir);
        s.execute("media.toggleProxies", json!({"enabled":use_proxy})).unwrap();
        open(&mut s, &manifest);
        assert_eq!(s.project.root.parent_of(source), None);
        let saved_tree = s.project.root.clone();
        s.execute("project.view.set", json!({"showReferenceClips":true})).unwrap();
        assert_eq!(s.execute("project.referenceClips", json!({"bin":cuts.0})).unwrap()["items"], json!([source.0]));
        assert_eq!(s.project.root, saved_tree);
        assert!(!s.is_dirty());
        assert_eq!(s.project.item(source).unwrap().name, source_name);
        assert!(render(&mut s) == pictures, "decoded pictures changed (proxy {use_proxy})");
    }
}
