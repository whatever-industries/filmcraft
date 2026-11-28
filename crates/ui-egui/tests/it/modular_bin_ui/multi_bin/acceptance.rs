//! Follow the same cut from conversion to crash recovery and a standard export.
use super::*;
use filmcraft_engine::autosave::AutosaveConfig;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const STAGE: &str = "FILMCRAFT_ACCEPTANCE_STAGE";
const ROOT: &str = "FILMCRAFT_ACCEPTANCE_ROOT";

#[derive(serde::Serialize, serde::Deserialize)]
struct Story {
    cuts: [Cut; 2],
    master: ItemId,
    nested: ClipId,
    at: Tick,
    master_start: Tick,
    camera: ItemId,
    protected: String,
}

fn manifest(root: &Path) -> PathBuf {
    root.join("origin/project/project.fcmod")
}
fn backup(root: &Path) -> PathBuf {
    root.join("backup/project.fcmod")
}
fn frame(s: &mut Session, reel: ItemId, at: Tick) -> Vec<u8> {
    s.execute("sequence.open", json!({"item":reel})).unwrap();
    s.try_render_program_at(0.125, at).unwrap().over_black_rgba8()
}
fn read_story(root: &Path) -> Story {
    serde_json::from_slice(&fs::read(root.join("story.json")).unwrap()).unwrap()
}

fn convert_and_save(root: &Path) {
    let standard = root.join("reference.fcproj");
    let cuts = prepare_standard(&standard);
    let [a, b] = cuts;
    let mut reference = open(&standard);
    let master = reference.project.sequences().map(|s| s.id).find(|id| *id != a.reel && *id != b.reel).unwrap();
    let original = root.join("origin/camera.png");
    let proxy = root.join("proxy.png");
    for (path, rgb) in [(&original, [20u8, 230, 60]), (&proxy, [10, 12, 250])] {
        image::RgbImage::from_pixel(160, 90, image::Rgb(rgb)).save(path).unwrap();
    }
    let media = reference.execute("file.import", json!({"paths":[original],"bin":b.bin})).unwrap();
    let camera = ItemId(media["items"][0].as_u64().unwrap());
    reference.execute("project.select", json!({"items":[camera]})).unwrap();
    reference.execute("media.attachProxies", json!({"item":camera,"path":proxy})).unwrap();
    let p = Arc::make_mut(&mut reference.project);
    let seq = p.sequence_mut(b.reel).unwrap();
    seq.video_tracks[0].items.iter_mut().find(|i| i.id == b.clip).unwrap().item = camera;
    let duration = p.sequence(a.reel).unwrap().duration();
    let rate = p.sequence(master).unwrap().settings.frame_rate;
    let master_start = rate.snap(p.sequence(master).unwrap().duration()) + rate.tick_of(1);
    let nested = p.make_track_item(a.reel, TrackKind::Video, master_start, TimeRange::new(Tick::ZERO, duration), rate).unwrap();
    let nested_id = nested.id;
    let track = &mut p.sequence_mut(master).unwrap().video_tracks[0];
    track.items.pop();
    track.items.push(nested);
    p.resolve_placed_auto_points(|_| true, |_| true);
    reference.execute("file.saveAs", json!({"path":standard})).unwrap();
    let manifest = manifest(root);
    project_folder::pack(&standard, manifest.parent().unwrap(), filmcraft_format::atomic_write_new).unwrap();
    let mut editor = open(&manifest);
    let protected = editor.folder_revision().unwrap().to_owned();
    editor.execute("project.keepRecoveryVersion", json!({"revision":protected,"keep":true})).unwrap();
    let at = a.boundary - reference.project.sequence(a.reel).unwrap().settings.frame_rate.tick_of(2);
    let story = Story { cuts, master, nested: nested_id, at, master_start, camera, protected };
    let historical_master = frame(&mut editor, master, master_start + at);
    assert!(historical_master.as_chunks::<4>().0.iter().any(|p| p[0] > 200 && p[1] < 100));
    for s in [&mut editor, &mut reference] {
        for c in cuts {
            c.trim(s);
        }
        s.execute("file.projectSettings.general", json!({"titleSafe":[12,14]})).unwrap();
        s.execute("file.save", json!({})).unwrap();
    }
    let mut reopened = open(&manifest);
    let mut reference = open(&standard);
    for c in cuts {
        assert_eq!(reopened.project.sequence(c.reel), reference.project.sequence(c.reel));
        assert!(c.pictures(&mut reopened) == c.pictures(&mut reference));
    }
    assert_eq!(reopened.project.settings.title_safe, (12.0, 14.0));
    assert_eq!(reopened.project.settings, reference.project.settings);
    project_folder::backup_history(manifest.parent().unwrap(), backup(root).parent().unwrap()).unwrap();
    // Control for the final unsaved edit, independent of the recovery or restore commands.
    a.trim(&mut reference);
    assert!(frame(&mut reference, master, master_start + at) != historical_master);
    reference.execute("file.save", json!({})).unwrap();
    fs::write(root.join("story.json"), serde_json::to_vec(&story).unwrap()).unwrap();
}

fn writer(root: &Path) {
    let a = read_story(root).cuts[0];
    let mut s = open(&manifest(root));
    s.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
    a.trim(&mut s);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        s.poll_persistence();
        if s.persistence.as_ref().unwrap().status.journaled_revision == s.revision {
            break;
        }
        assert!(Instant::now() < deadline, "checkpoint overdue");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(s.is_dirty());
    assert!(s.dirty_folder_bins().contains(&a.bin));
    println!("checkpoint confirmed");
    std::thread::park();
}

fn verify(root: &Path, s: &mut Session, story: &Story, copy: ItemId) {
    let [a, b] = story.cuts;
    let mut reference = open(&root.join("reference.fcproj"));
    let saved = s.project.sequence(copy).unwrap().video_tracks[0].items.iter().find(|i| i.id == a.clip).unwrap();
    let current = s.project.sequence(a.reel).unwrap().video_tracks[0].items.iter().find(|i| i.id == a.clip).unwrap();
    assert!(saved.duration > current.duration);
    assert_eq!(s.project.settings, reference.project.settings);
    assert_eq!(s.project.sequence(b.reel), reference.project.sequence(b.reel));
    let nested = s.project.sequence(story.master).unwrap().video_tracks[0].items.iter().find(|i| i.id == story.nested).unwrap();
    assert_eq!(nested.item, a.reel);
    let today = frame(s, a.reel, story.at);
    assert!(today == frame(&mut reference, a.reel, story.at));
    let old = frame(s, copy, story.at);
    assert!(s.state.open_sequences.contains(&a.reel) && s.state.open_sequences.contains(&copy));
    assert!(today != old && old.as_chunks::<4>().0.iter().any(|p| p[0] > 200 && p[1] < 100));
    for t in [story.at, story.at - reference.project.sequence(a.reel).unwrap().settings.frame_rate.tick_of(6)] {
        assert!(frame(s, story.master, story.master_start + t) == frame(&mut reference, story.master, story.master_start + t));
    }
    for editor in [&*s, &reference] {
        assert!(editor.offline.prompt && editor.offline.missing.contains(&story.camera));
    }
    for on in [false, true] {
        let at = b.boundary - reference.project.sequence(b.reel).unwrap().settings.frame_rate.frame_duration();
        for editor in [&mut *s, &mut reference] {
            editor.execute("media.toggleProxies", json!({"enabled":on})).unwrap();
        }
        let picture = frame(s, b.reel, at);
        assert!(picture == frame(&mut reference, b.reel, at));
        assert_eq!(picture.as_chunks::<4>().0.iter().any(|p| p[2] > 220 && p[0] < 50), on);
    }
}

fn recover(root: &Path) {
    assert!(!manifest(root).exists());
    let story = read_story(root);
    let a = story.cuts[0];
    let mut s = Session::local_folder_editor();
    s.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
    s.execute("file.recover", json!({})).unwrap();
    assert!(s.path.is_none() && s.is_dirty());
    let mut reference = open(&root.join("reference.fcproj"));
    assert!(frame(&mut s, a.reel, story.at) == frame(&mut reference, a.reel, story.at));
    let path = backup(root);
    let head = FolderDirectory::published_revision(&path).unwrap();
    let p = json!({"backupPath":path,"revision":story.protected,"sequence":a.reel});
    let unrelated = root.join("unrelated");
    project_folder::pack(&root.join("reference.fcproj"), &unrelated, filmcraft_format::atomic_write_new).unwrap();
    let mut wrong = p.clone();
    wrong["backupPath"] = json!(unrelated.join("project.fcmod"));
    let before = s.revision;
    assert!(s.execute("project.restoreSequenceCopy", wrong).is_err());
    assert_eq!(s.revision, before);
    let versions = s.execute("project.recoveryVersions", p.clone()).unwrap();
    assert!(versions["versions"].as_array().unwrap().iter().any(|v| v["revision"] == story.protected));
    let seqs = s.execute("project.recoverySequences", p.clone()).unwrap();
    assert!(seqs["sequences"].as_array().unwrap().iter().any(|v| v[0] == json!(a.reel)));
    // A changed dependency can still be rescued separately using the original media paths.
    let mut selection = p.clone();
    selection["sequence"] = json!(story.cuts[1].reel);
    s.execute("clip.rename", json!({"item":story.camera,"name":"Changed camera"})).unwrap();
    let before = s.revision;
    assert_eq!(s.execute("project.restoreSequenceCopy", selection.clone()).unwrap()["exportRequired"], true);
    let rescue = root.join("separate.fcproj");
    selection["path"] = json!(rescue);
    s.execute("project.exportRecoverySequence", selection).unwrap();
    assert_eq!(s.revision, before);
    let rescued = open(&rescue);
    assert!(rescued.offline.missing.contains(&story.camera));
    let source = rescued.project.item(story.camera).unwrap().as_media().unwrap();
    assert_eq!(source.media, reference.project.item(story.camera).unwrap().as_media().unwrap().media);
    s.execute("edit.undo", json!({})).unwrap();
    let result = s.execute("project.restoreSequenceCopy", p).unwrap();
    let copy = ItemId(result["item"].as_u64().expect("expected restored copy"));
    assert_ne!(copy, a.reel);
    assert_eq!(s.project.root.parent_of(copy), s.project.root.parent_of(a.reel));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(copy).is_none());
    s.execute("edit.redo", json!({})).unwrap();
    verify(root, &mut s, &story, copy);
    s.execute("file.saveAs", json!({"path":root.join("export.fcproj")})).unwrap();
    assert_eq!(FolderDirectory::published_revision(&path).unwrap(), head);
    fs::write(root.join("copy.json"), serde_json::to_vec(&copy).unwrap()).unwrap();
}

#[test]
fn recovered_edit_and_protected_copy_reopen_together() {
    if let Some(stage) = std::env::var_os(STAGE) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        match stage.to_str().unwrap() {
            "prepare" => convert_and_save(&root),
            "edit" => writer(&root),
            "recover" => recover(&root),
            "reopen" => verify(
                &root,
                &mut open(&root.join("export.fcproj")),
                &read_story(&root),
                serde_json::from_slice(&fs::read(root.join("copy.json")).unwrap()).unwrap(),
            ),
            _ => panic!("unknown acceptance stage"),
        }
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("origin")).unwrap();
    convert_and_save(root);
    let child = |stage: &str| {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "modular_bin_ui::multi_bin::acceptance::recovered_edit_and_protected_copy_reopen_together", "--nocapture"])
            .env(STAGE, stage)
            .env(ROOT, root);
        cmd
    };
    let mut editor = child("edit").stdout(Stdio::piped()).spawn().unwrap();
    let confirmed = BufReader::new(editor.stdout.take().unwrap()).lines().map_while(Result::ok).any(|l| l.contains("checkpoint confirmed"));
    if confirmed {
        editor.kill().unwrap();
    }
    editor.wait().unwrap();
    assert!(confirmed);
    fs::rename(root.join("origin"), root.join("disconnected")).unwrap();
    for stage in ["recover", "reopen"] {
        assert!(child(stage).status().unwrap().success());
    }
}
