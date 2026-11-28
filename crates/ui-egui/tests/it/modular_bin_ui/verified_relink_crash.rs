//! Kill a desktop session after a durable worker acknowledgement. Recovery must
//! include deferred timelines and use ordinary project media resolution.
use super::*;
use filmcraft_engine::autosave::AutosaveConfig;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const CHILD: &str = "FILMCRAFT_TEST_DESKTOP_RELINK_ACTION";
const ROOT: &str = "FILMCRAFT_TEST_DESKTOP_RELINK_ROOT";

fn command(root: &Path, stage: &str) -> Command {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "modular_bin_ui::verified_relink::crash::crash_after_warm_relink_or_promotion", "--nocapture"]).env(CHILD, stage).env(ROOT, root);
    child
}

fn recover_detached(root: &Path) {
    let origin = root.join("origin");
    assert!(!origin.join("Feature/project.fcmod").exists());
    let mut s = Session::local_folder_editor();
    s.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
    assert_eq!(s.recovery_candidates().len(), 1);
    let draft = filmcraft_engine::autosave::load_candidate(&s.recovery_candidates()[0]).unwrap();
    let reference = filmcraft_format::decode(&fs::read(root.join("expected.fcproj")).unwrap()).unwrap();
    assert_eq!(draft.project, reference.project);
    let (source, master, deferred): (ItemId, ItemId, ItemId) = serde_json::from_slice(&fs::read(root.join("ids.json")).unwrap()).unwrap();
    s.execute("file.recover", json!({})).unwrap();
    assert!(s.is_dirty());
    assert_eq!(s.project.settings, reference.project.settings);
    assert_eq!(s.project.sequence(deferred), reference.project.sequence(deferred));
    let media = s.project.item(source).unwrap().as_media().unwrap();
    assert!(!media.offline);
    assert!(s.offline.missing.contains(&source));
    assert!(s.offline.prompt);
    assert_eq!(media.media, filmcraft_project::MediaRef::File { path: origin.join("Moved/camera.png").to_string_lossy().into_owned() });
    let mut standard = open(&root.join("expected.fcproj"));
    s.state.active_sequence = Some(master);
    standard.state.active_sequence = Some(master);
    for enabled in [true, false] {
        for editor in [&mut s, &mut standard] {
            editor.execute("media.toggleProxies", json!({"enabled":enabled})).unwrap();
        }
        let frame = picture(&s);
        assert!(frame == picture(&standard));
        assert_eq!(frame.as_chunks::<4>().0.iter().any(|px| px[2] > 220 && px[0] < 30), enabled);
    }
    let saved = root.join("recovered.fcproj");
    s.execute("file.saveAs", json!({"path":saved})).unwrap();
    fs::rename(root.join("unmounted"), &origin).unwrap();
    let reopened = open(&saved);
    assert!(!reopened.offline.missing.contains(&source));
    assert_eq!(reopened.project.item(source).unwrap().as_media(), s.project.item(source).unwrap().as_media());
}

fn write_until_ack(root: &Path, stage: &str) {
    let origin = root.join("origin");
    fs::create_dir_all(&origin).unwrap();
    let src = Source::new(&origin);
    if stage == "warm" {
        let proxy = root.join("proxy.png");
        image::RgbImage::from_pixel(160, 90, image::Rgb([10u8, 12, 250])).save(&proxy).unwrap();
        let mut full = open(&src.f.path);
        full.execute("project.select", json!({"items":[src.id]})).unwrap();
        full.execute("media.attachProxies", json!({"item":src.id,"path":proxy})).unwrap();
        full.execute("file.save", json!({})).unwrap();
    }
    let data = root.join("data");
    let (mut s, deferred) = warm_source(&src, &data);
    let mut cfg = AutosaveConfig::new(&data);
    cfg.journal_debounce = Duration::from_millis(30);
    s.start_autosave(cfg).unwrap();
    let review = src.discover(&mut s);
    src.apply(&mut s, &review);
    assert!(s.has_deferred_timelines());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        s.poll_persistence();
        if s.persistence.as_ref().unwrap().status.journaled_revision == s.revision {
            break;
        }
        assert!(Instant::now() < deadline, "background checkpoint did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(s.is_dirty());
    if stage == "redo" {
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("file.projectSettings.general", json!({})).unwrap();
        s.execute("edit.redo", json!({})).unwrap();
    }
    if matches!(stage, "promote" | "save-other" | "save-source" | "discard-source") {
        s.execute("clip.rename", json!({"item":src.f.master,"name":"Today's cut"})).unwrap();
        if stage == "save-other" {
            s.execute("project.saveBin", json!({"bin":src.f.cuts})).unwrap();
        }
        if stage == "save-source" {
            s.execute("project.saveBin", json!({"bin":src.f.selects})).unwrap();
        }
        if stage == "discard-source" {
            s.execute("project.discardBin", json!({"bin":src.f.selects})).unwrap();
        }
    }
    if stage == "discard" {
        s.execute("project.discardBin", json!({"bin":src.f.selects})).unwrap();
    }
    let ack = s.persistence.as_ref().unwrap().flush().unwrap();
    assert_eq!(ack, s.is_dirty().then_some(s.revision));
    fs::write(root.join("ids.json"), serde_json::to_vec(&(src.id, src.f.master, deferred)).unwrap()).unwrap();
    if stage == "warm" {
        let (_, mut reference) = filmcraft_format::project_folder::FolderSnapshot::open(&src.f.path).unwrap();
        *reference.project.item_mut(src.id).unwrap() = s.project.item(src.id).unwrap().clone();
        fs::write(root.join("expected.fcproj"), filmcraft_format::encode(&reference.project, false)).unwrap();
    }
    println!("desktop relink is durable");
    std::thread::park();
}

#[test]
fn crash_after_warm_relink_or_promotion() {
    if let Ok(stage) = std::env::var(CHILD) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        if stage == "detached" {
            recover_detached(&root);
        } else {
            write_until_ack(&root, &stage);
        }
        return;
    }
    for stage in ["warm", "promote", "redo", "save-other", "save-source", "discard-source", "discard"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut child = command(root, stage).stdout(Stdio::piped()).spawn().unwrap();
        let ready = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|line| line.contains("desktop relink is durable"));
        if ready {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        assert!(ready, "{stage} exited before reaching its recovery boundary");
        let (source, master, deferred): (ItemId, ItemId, ItemId) = serde_json::from_slice(&fs::read(root.join("ids.json")).unwrap()).unwrap();
        if stage == "warm" {
            fs::rename(root.join("origin"), root.join("unmounted")).unwrap();
            let status = command(root, "detached").status().unwrap();
            assert!(status.success());
            continue;
        }
        let disk = open(&root.join("origin/Feature/project.fcmod"));
        assert_eq!(disk.project.item(master).unwrap().name == "Today's cut", stage == "save-other");
        let mut s = Session::local_folder_editor();
        s.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
        if stage == "discard" {
            assert!(s.recovery_candidates().is_empty());
            continue;
        }
        assert_eq!(s.recovery_candidates().len(), 1);
        let unsaved_relink = !matches!(stage, "save-source" | "discard-source");
        assert_eq!(s.recovery_candidates()[0].meta.reviewed_source, unsaved_relink.then_some(source));
        let chosen = root.join("origin/Moved/camera.png");
        image::RgbImage::from_pixel(160, 90, image::Rgb([5u8, 188, 10])).save(&chosen).unwrap();
        s.execute("file.recover", json!({})).unwrap();
        assert!(s.project.sequence(deferred).is_some());
        assert_eq!(s.project.item(master).unwrap().name == "Today's cut", !matches!(stage, "warm" | "redo"));
        if unsaved_relink {
            let media = s.project.item(source).unwrap().as_media().unwrap();
            assert!(!media.offline);
            assert!(!s.offline.missing.contains(&source));
            assert_eq!(media.media, filmcraft_project::MediaRef::File { path: chosen.to_string_lossy().into_owned() });
            let saved = root.join("recovered.fcproj");
            s.execute("file.saveAs", json!({"path":saved})).unwrap();
            let mut reopened = open(&saved);
            reopened.state.active_sequence = Some(master);
            assert!(!reopened.offline.missing.contains(&source));
            assert!(picture(&reopened).as_chunks::<4>().0.iter().any(|px| px[1] > 150 && px[0] < 30));
        }
    }
}
