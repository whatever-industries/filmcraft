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
    check_shared_source(dir, &stock, source, sources, cuts, |s| assert!(render(s) == expected));
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

fn check_shared_source(dir: &Path, stock: &Path, source: ItemId, sources: BinId, cuts: BinId, verify_playback: impl Fn(&mut Session)) {
    // Two independent editors share a source, even though only one bin owns its home filing.
    let collaboration = dir.join("collaboration");
    pack(stock, &collaboration, filmcraft_format::atomic_write_new).unwrap();
    let head = collaboration.join(ROOT);
    reverse_save_order(dir, &head, source, sources, cuts);
    let duplicate;
    {
        let mut b = editor(dir);
        open(&mut b, &head);
        b.open_desktop_bin(cuts.0).unwrap();
        let item = b.project.item(source).unwrap();
        assert_eq!(item.metadata["Comments"], "approved");
        assert_eq!(item.name, "Approved interview");
        assert_eq!(item.custom_label_color, filmcraft_project::HexColor::parse("#BBCCDD"));
        assert_eq!(item.metadata["Scene"], "kitchen");
        assert_eq!(item.metadata["Camera"], "B");
        assert!(!item.metadata.contains_key("Take"));
        assert_eq!(b.project.root.parent_of(source), Some(sources));
        b.execute("project.removeFromBin", json!({"bin":cuts.0,"items":[source.0]})).unwrap();
        assert!(crate::project_panel::reference_clips(&b.project, b.project.root.find_bin(cuts).unwrap()).contains(&source));
        assert_eq!(b.project.root.parent_of(source), Some(sources));
        b.execute("edit.undo", json!({})).unwrap();
        let cut_bin = b.project.root.find_bin(cuts).unwrap();
        assert_eq!(cut_bin.filed_sources, [source]);
        assert!(!crate::project_panel::reference_clips(&b.project, cut_bin).contains(&source));
        b.execute("project.select", json!({"items":[source.0]})).unwrap();
        let ids = b.execute("edit.duplicate", json!({})).unwrap();
        duplicate = filmcraft_project::ItemId(ids["items"][0].as_u64().unwrap());
        assert_ne!(duplicate, source);
        assert_eq!(b.project.item(duplicate).unwrap().metadata, b.project.item(source).unwrap().metadata);
        b.execute("file.save", json!({})).unwrap();
        set_fields(&mut b, duplicate, json!({"Comments":"independent copy"}));
        set_fields(&mut b, source, json!({"Camera":"A"}));
        b.execute("file.save", json!({})).unwrap();
    }
    {
        let mut s = editor(dir);
        open(&mut s, &head);
        for bin in [cuts, sources] {
            assert!(crate::project_panel::bin_children(&s.project, s.project.root.find_bin(bin).unwrap(), &Default::default()).1.contains(&source));
        }
        assert_eq!(s.project.item(source).unwrap().metadata["Comments"], "approved");
        assert_eq!(s.project.item(duplicate).unwrap().metadata["Comments"], "independent copy");
        assert_eq!(s.project.item(source).unwrap().metadata["Camera"], "A");
        assert_eq!(s.project.item(duplicate).unwrap().metadata["Camera"], "B");
        verify_playback(&mut s);
        let dest = dir.join("collaboration-export.fcproj");
        s.execute("file.saveCopy", json!({"path":dest})).unwrap();
        let bytes = fs::read(&dest).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("\"filed_sources\"") && !text.contains("\"field_edits\""));
        let mut ordinary = Session::default();
        open(&mut ordinary, &dest);
        assert_eq!(ordinary.project.root.parent_of(source), Some(sources));
        verify_playback(&mut ordinary);
    }
}

fn set_fields(s: &mut Session, id: ItemId, fields: Value) {
    s.execute("metadata.set", json!({"item":id.0,"fields":fields})).unwrap();
}

fn reverse_save_order(dir: &Path, head: &Path, source: ItemId, sources: BinId, cuts: BinId) {
    {
        let mut a = editor(&dir.join("first-editor"));
        let mut b = editor(&dir.join("second-editor"));
        open(&mut a, head);
        open(&mut b, head);
        a.open_desktop_bin(sources.0).unwrap();
        b.open_desktop_bin(cuts.0).unwrap();
        let cut_bin = b.project.root.find_bin(cuts).unwrap();
        assert!(crate::project_panel::reference_clips(&b.project, cut_bin).contains(&source));
        assert!(!crate::project_panel::bin_children(&b.project, cut_bin, &Default::default()).1.contains(&source));
        for changed in [true, false] {
            assert_eq!(b.execute("project.fileInBin", json!({"item":source.0,"bin":cuts.0})).unwrap()["changed"], changed);
        }
        set_fields(&mut a, source, json!({"Comments":"older","Scene":"kitchen","Take":"first","Name":"first pass","Label":"#223344"}));
        set_fields(&mut b, source, json!({"Comments":"approved","Camera":"B","Take":"second","Name":"Approved interview"}));
        b.execute("edit.label", json!({"items":[source.0],"label":"#BBCCDD"})).unwrap();
        set_fields(&mut b, source, json!({"Take":""}));
        b.execute("file.save", json!({})).unwrap();
        a.execute("file.save", json!({})).unwrap();
        assert_eq!(a.project.item(source).unwrap().metadata["Comments"], "approved");
        assert!(!a.project.item(source).unwrap().metadata.contains_key("Take"));
        a.execute("edit.undo", json!({})).unwrap();
        assert_eq!(a.project.item(source).unwrap().metadata["Comments"], "approved");
        assert!(!a.project.item(source).unwrap().metadata.contains_key("Scene"));
        a.execute("file.save", json!({})).unwrap();
        a.execute("edit.redo", json!({})).unwrap();
        a.execute("file.save", json!({})).unwrap();
        // A competing reader cannot remove the second filing.
        assert!(a.execute("project.removeFromBin", json!({"bin":cuts.0,"items":[source.0]})).is_err());
    }
}

#[test]
fn browse_saved_bin_uses_revision_values_and_keeps_live_checkout_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let path = fixture(temp.path());
    let mut s = editor(temp.path());
    open(&mut s, &path);
    let rev = s.execute("project.binHistory", json!({"bin":70})).unwrap()["revisions"][0][1].clone();
    rename(&mut s, 70, "Today's cuts");
    s.execute("file.save", json!({})).unwrap();
    let head = fs::read(&path).unwrap();
    let listing = s.execute("project.binHistory", json!({"revision":rev})).unwrap();
    assert!(listing["bins"].as_array().unwrap().iter().any(|r| r["id"] == 70 && r["name"] == "A"));
    let page = s.execute("project.binHistory", json!({"revision":rev,"bin":70})).unwrap();
    assert_eq!(page["name"], "A");
    let rows = page["rows"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["item"] == 21 && r["reference"] == false));
    assert!(rows.iter().any(|r| r["item"] == 5 && r["reference"] == true));
    assert_eq!(fs::read(path).unwrap(), head);
    assert_eq!(name(&s, 70), "Today's cuts");
}

#[test]
fn historical_paste_preserves_identity_extensions_and_one_step_undo_across_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let path = fixture(dir);
    let mut s = editor(dir);
    open(&mut s, &path);
    let rev = s.execute("project.binHistory", json!({})).unwrap()["revisions"][0][1].clone();
    s.open_desktop_bin(70).unwrap();
    s.execute("metadata.set", json!({"item":5,"bin":70,"fields":{"Comments":"new live notes"}})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    let paste = |s: &mut Session, items: Value| {
        let epoch = s.document_epoch;
        s.execute("project.pasteFromHistory", json!({"bin":71,"revision":rev,"epoch":epoch,"items":items}))
    };
    let recover = |s: &mut Session| {
        let r = paste(s, json!([21])).unwrap();
        ItemId(r["items"][0].as_u64().unwrap())
    };
    let source = |s: &Session, id| s.project.sequence(id).unwrap().video_tracks[0].items[0].item;
    s.open_desktop_bin(71).unwrap();
    let first = recover(&mut s);
    let historical = source(&s, first);
    assert_ne!(first, ItemId(21));
    assert_ne!(historical, ItemId(5));
    s.execute("file.save", json!({})).unwrap();
    drop(s);
    let mut s = editor(dir);
    open(&mut s, &path);
    s.open_desktop_bin(71).unwrap();
    let second = recover(&mut s);
    assert_ne!(second, first);
    assert_eq!(source(&s, second), historical);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(second).is_none());
    assert_eq!(source(&s, first), historical);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(source(&s, second), historical);
    paste(&mut s, json!([5])).unwrap();
    s.execute("project.select", json!({"items":[historical]})).unwrap();
    let duplicate = s.execute("edit.duplicate", json!({"bin":71})).unwrap()["items"][0].as_u64().unwrap();
    s.execute("metadata.set", json!({"item":historical,"bin":71,"fields":{"Comments":"my recovered notes"}})).unwrap();
    let third = recover(&mut s);
    assert_ne!(source(&s, third), historical);
    assert_ne!(source(&s, third), ItemId(duplicate));
    assert_eq!(source(&s, first), historical);
    assert_eq!(s.project.item(historical).unwrap().metadata["Comments"], "my recovered notes");
    assert_eq!(s.project.item(source(&s, third)).unwrap().metadata.get("Comments"), s.project.item(ItemId(duplicate)).unwrap().metadata.get("Comments"));
    let before = s.project.clone();
    assert!(paste(&mut s, json!([21, 999999])).is_err());
    assert_eq!(s.project, before);
    s.execute("file.save", json!({})).unwrap();
    let output = dir.join("cuts.fcproj");
    s.execute("file.saveCopy", json!({"path":output})).unwrap();
    let bytes = fs::read(output).unwrap();
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    for id in [first, second, third] {
        assert_eq!(doc["project"]["items"][id.0.to_string()]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["extension"]["camera"], "A");
    }
    let stock = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(stock.item(historical).unwrap().metadata["Comments"], "my recovered notes");
    assert_eq!(stock.item(ItemId(5)).unwrap().metadata["Comments"], "new live notes");
}

#[test]
fn historical_source_is_shared_between_two_writable_bins_before_save() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    let path = fixture(dir);
    let mut s = editor(dir);
    open(&mut s, &path);
    let listing = s.execute("project.binHistory", json!({})).unwrap();
    let rev = &listing["revisions"][0][1];
    let epoch = &listing["epoch"];
    s.execute("metadata.set", json!({"item":5,"bin":70,"fields":{"Comments":"new notes"}})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    let mut recover = |bin| {
        s.open_desktop_bin(bin).unwrap();
        let r = s.execute("project.pasteFromHistory", json!({"epoch":epoch,"revision":rev,"bin":bin,"items":[21]})).unwrap();
        ItemId(r["items"][0].as_u64().unwrap())
    };
    let first = recover(71);
    let second = recover(70);
    let source = |s: &Session, id| s.project.sequence(id).unwrap().video_tracks[0].items[0].item;
    let historical = source(&s, first);
    assert_ne!(historical, ItemId(5));
    assert_eq!(source(&s, second), historical);
    let export = dir.join("both.fcproj");
    s.execute("file.saveCopy", json!({"path":export})).unwrap();
    let stock = filmcraft_format::decode(&fs::read(export).unwrap()).unwrap().project;
    assert_eq!(stock.sequence(second).unwrap().video_tracks[0].items[0].item, historical);
    assert_eq!(s.save_open_bins().unwrap()["savedBins"], json!([71, 70]));
    drop(s);
    let mut s = editor(dir);
    open(&mut s, &path);
    assert_eq!(source(&s, first), historical);
    assert_eq!(source(&s, second), historical);
}

#[test]
fn historical_batch_paste_plays_both_trims_and_preserves_proxy_after_reopen() {
    use crate::media_test_util::{make_movie, session_with};
    use filmcraft_media::DemoScene;
    use filmcraft_project::TrackKind;
    use filmcraft_time::{FrameRate, Tick, TimeRange};
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    let original = dir.join("Interview.mov");
    let proxy = dir.join("Interview Proxy.mov");
    make_movie(&original, DemoScene::Aurora, 160, 96, 12);
    make_movie(&proxy, DemoScene::Plasma, 80, 48, 12);
    let (mut setup, sources, first) = session_with(&[&original]);
    let source = sources[0];
    setup.execute("media.attachProxies", json!({"item":source,"path":proxy})).unwrap();
    let project = Arc::make_mut(&mut setup.project);
    let cuts = project.add_bin("Cuts", None);
    let dest = project.add_bin("Recovered", None);
    project.move_to_bin(&[first], Some(cuts)).unwrap();
    let second = project.new_sequence("Trailer", project.sequence(first).unwrap().settings.clone(), 1, 1, Some(cuts));
    let rate = FrameRate::FPS_24;
    let trimmed = TimeRange::new(rate.tick_of(4), rate.tick_of(7));
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let clip = project.make_track_item(source, kind, Tick::ZERO, trimmed, rate).unwrap();
        let seq = project.sequence_mut(second).unwrap();
        match kind {
            TrackKind::Video => seq.video_tracks[0].items.push(clip),
            TrackKind::Audio => seq.audio_tracks[0].items.push(clip),
        }
    }
    let playback = |s: &mut Session, ids: [ItemId; 2]| {
        let mut samples = Vec::new();
        for id in ids {
            s.execute("sequence.open", json!({"item":id})).unwrap();
            let (image, offline) = s.try_render_program_with_offline_at(1.0, rate.tick_of(1)).unwrap();
            assert!(offline.is_empty());
            let provider = s.media.provider(s.project.clone(), s.services.clone());
            let audio = filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), 2000, 1000, &provider);
            assert!(audio.channels.iter().flatten().any(|v| v.abs() > 0.001));
            samples.push((image.over_black_rgba8(), audio.channels));
        }
        samples
    };
    let expected = playback(&mut setup, [first, second]);
    assert!(expected[0].0 != expected[1].0);
    setup.execute("media.toggleProxies", json!({"enabled":true})).unwrap();
    let expected_proxy = playback(&mut setup, [first, second]);
    assert!(expected[0].0 != expected_proxy[0].0);
    let stock = dir.join("draft.fcproj");
    setup.execute("file.saveAs", json!({"path":stock})).unwrap();
    drop(setup);
    let folder = dir.join("modular");
    pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let path = folder.join(ROOT);
    let mut s = editor(dir);
    open(&mut s, &path);
    let rev = s.execute("project.binHistory", json!({})).unwrap()["revisions"][0][1].clone();
    s.open_desktop_bin(dest.0).unwrap();
    let args = json!({"epoch":s.document_epoch,"revision":rev,"bin":dest,"items":[first,second]});
    let mut peer = editor(&dir.join("peer"));
    open(&mut peer, &path);
    assert_eq!(peer.open_desktop_bin(dest.0).unwrap()["readOnly"], true);
    assert!(peer.execute("project.pasteFromHistory", args.clone()).is_err());
    let before = fs::read(&path).unwrap();
    let rows = s.execute("project.pasteFromHistory", args).unwrap();
    let ids: [ItemId; 2] = serde_json::from_value(rows["items"].clone()).unwrap();
    assert!(!ids.contains(&first) && !ids.contains(&second) && ids[0] != ids[1]);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(s.project.root.find_bin(dest).unwrap().children.len(), 2);
    for id in ids {
        assert_eq!(s.project.sequence(id).unwrap().video_tracks[0].items[0].item, source);
    }
    s.execute("file.save", json!({})).unwrap();
    drop(peer);
    drop(s);
    // A later metadata change makes the original source unsuitable for yesterday's cuts.
    let restored = |s: &mut Session, cut: ItemId| {
        s.open_desktop_bin(dest.0).unwrap();
        let result = s.execute("project.pasteFromHistory", json!({"epoch":s.document_epoch,"revision":rev,"bin":dest,"items":[cut]})).unwrap();
        ItemId(result["items"][0].as_u64().unwrap())
    };
    let mut writer = editor(dir);
    open(&mut writer, &path);
    writer.execute("metadata.set", json!({"bin":cuts,"item":source,"fields":{"Comments":"today's notes"}})).unwrap();
    writer.execute("file.save", json!({})).unwrap();
    let rough = restored(&mut writer, first);
    let saved_source = writer.project.sequence(rough).unwrap().video_tracks[0].items[0].item;
    assert_ne!(saved_source, source);
    writer.execute("file.save", json!({})).unwrap();
    drop(writer);
    let mut writer = editor(dir);
    open(&mut writer, &path);
    let trailer = restored(&mut writer, second);
    assert_eq!(writer.project.sequence(trailer).unwrap().video_tracks[0].items[0].item, saved_source);
    writer.execute("file.save", json!({})).unwrap();
    drop(writer);
    let mut writer = editor(dir);
    open(&mut writer, &path);
    writer.execute("metadata.set", json!({"bin":dest,"item":saved_source,"fields":{"Comments":"approved recovered source"}})).unwrap();
    let alternate = restored(&mut writer, second);
    let alternate_source = writer.project.sequence(alternate).unwrap().video_tracks[0].items[0].item;
    assert_ne!(alternate_source, saved_source);
    writer.execute("file.save", json!({})).unwrap();
    drop(writer);
    for proxy_only in [false, true] {
        if proxy_only {
            fs::rename(&original, dir.join("disconnected.mov")).unwrap();
        }
        let mut s = editor(dir);
        s.execute("media.toggleProxies", json!({"enabled":proxy_only})).unwrap();
        open(&mut s, &path);
        let pictures = if proxy_only { &expected_proxy } else { &expected };
        assert!(playback(&mut s, ids) == *pictures);
        assert!(playback(&mut s, [rough, trailer]) == *pictures);
        assert!(playback(&mut s, [rough, alternate]) == *pictures);
        assert_eq!(s.project.item(saved_source).unwrap().metadata["Comments"], "approved recovered source");
        assert_ne!(s.project.item(alternate_source).unwrap().metadata.get("Comments"), s.project.item(saved_source).unwrap().metadata.get("Comments"));
    }
}

#[test]
fn compare_bin_versions_and_recover_five_frame_trim_after_reopen() {
    use crate::media_test_util::{make_movie, session_with};
    use filmcraft_media::DemoScene;
    let dir = tempfile::tempdir().unwrap();
    let movie = dir.path().join("Interview.mov");
    make_movie(&movie, DemoScene::Aurora, 160, 96, 12);
    let (mut setup, sources, cut) = session_with(&[&movie]);
    let source = sources[0];
    let project = Arc::make_mut(&mut setup.project);
    let cuts = project.add_bin("Cuts", None);
    let recovered = project.add_bin("Recovered", None);
    project.move_to_bin(&[cut], Some(cuts)).unwrap();
    let stock = dir.path().join("draft.fcproj");
    setup.execute("file.saveAs", json!({"path":stock})).unwrap();
    let folder = dir.path().join("folder");
    pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let path = folder.join(ROOT);
    let rate = setup.sequence_rate();
    let frame = |s: &mut Session, id, n| {
        s.execute("sequence.open", json!({"item":id})).unwrap();
        let (img, offline) = s.try_render_program_with_offline_at(1.0, rate.tick_of(n)).unwrap();
        assert!(offline.is_empty());
        img.over_black_rgba8()
    };
    let last_shared = frame(&mut setup, cut, 6);
    let old_tail = frame(&mut setup, cut, 7);
    drop(setup);
    let (mut snapshot, before) = filmcraft_format::project_folder::FolderSnapshot::open(&path).unwrap();
    let mut project = filmcraft_format::decode(&before).unwrap().project;
    project.root.find_bin_mut(cuts).unwrap().name = "Fine cuts".into();
    let seq = project.sequence_mut(cut).unwrap();
    for track in [&mut seq.video_tracks[0], &mut seq.audio_tracks[0]] {
        assert_eq!(rate.frame_at(track.items[0].duration), 12);
        track.items[0].duration = rate.tick_of(7);
    }
    snapshot.save(&before, &filmcraft_format::encode(&project, false)).unwrap();
    drop(snapshot);
    let mut s = editor(dir.path());
    open(&mut s, &path);
    let revs = s.execute("project.binHistory", json!({})).unwrap()["revisions"].clone();
    let undo_depth = s.history.undo.len();
    let revision = s.revision;
    let summary = s.execute("project.compareBinVersions", json!({"bin":cuts,"to":revs[0][1]})).unwrap();
    assert_eq!(summary["from"], revs[1][1]);
    assert_eq!(summary["report"]["before_name"], "Cuts");
    assert_eq!(summary["report"]["after_name"], "Fine cuts");
    let report = s.execute("project.compareBinVersions", json!({"bin":cuts,"sequence":cut,"from":summary["from"],"to":summary["to"]})).unwrap();
    let rows = report["report"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r["changes"] == json!(["trimmed"]) && r["before"]["durationFrames"] == 12 && r["after"]["durationFrames"] == 7));
    assert_eq!((s.revision, s.history.undo.len()), (revision, undo_depth));
    assert!(rows.iter().all(|r| ["track", "start", "sourceIn", "source"].iter().all(|k| r["before"][*k] == r["after"][*k])));
    s.open_desktop_bin(recovered.0).unwrap();
    let copy = s.execute("project.pasteFromHistory", json!({"epoch":s.document_epoch,"revision":report["from"],"bin":recovered,"items":[cut]})).unwrap();
    let rescued: ItemId = serde_json::from_value(copy["items"][0].clone()).unwrap();
    assert_ne!(rescued, cut);
    let second = s.execute("project.pasteFromHistory", json!({"epoch":s.document_epoch,"revision":report["to"],"bin":recovered,"items":[cut]})).unwrap();
    let short: ItemId = serde_json::from_value(second["items"][0].clone()).unwrap();
    assert!(short != cut && short != rescued);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(short).is_none() && s.project.item(rescued).is_some());
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    drop(s);
    let mut s = editor(dir.path());
    open(&mut s, &path);
    for (id, length) in [(cut, 7), (short, 7), (rescued, 12)] {
        let clip = &s.project.sequence(id).unwrap().video_tracks[0].items[0];
        assert_eq!(clip.item, source);
        assert_eq!(rate.frame_at(clip.duration), length);
        assert!(frame(&mut s, id, 6) == last_shared);
    }
    assert!(frame(&mut s, rescued, 7) == old_tail);
    assert!(frame(&mut s, cut, 7) != old_tail);
}
