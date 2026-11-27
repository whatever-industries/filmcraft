//! Kill a desktop session after a durable worker acknowledgement. Recovery must
//! include deferred timelines and must recheck the file chosen during review.
use super::*;
use filmcraft_engine::autosave::AutosaveConfig;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn crash_after_warm_relink_or_promotion() {
    const CHILD: &str = "FILMCRAFT_TEST_DESKTOP_RELINK_ACTION";
    if let Ok(stage) = std::env::var(CHILD) {
        let root = PathBuf::from(std::env::var_os("FILMCRAFT_TEST_DESKTOP_RELINK_ROOT").unwrap());
        let src = Source::new(&root);
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
        if matches!(stage.as_str(), "promote" | "save-other" | "save-source" | "discard-source") {
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
        println!("desktop relink is durable");
        std::thread::park();
        return;
    }
    for stage in ["warm", "promote", "redo", "save-other", "save-source", "discard-source", "discard"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "modular_bin_ui::verified_relink::crash::crash_after_warm_relink_or_promotion", "--nocapture"])
            .env(CHILD, stage)
            .env("FILMCRAFT_TEST_DESKTOP_RELINK_ROOT", root)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let ready = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|line| line.contains("desktop relink is durable"));
        if ready {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        assert!(ready, "{stage} exited before reaching its recovery boundary");
        let (source, master, deferred): (ItemId, ItemId, ItemId) = serde_json::from_slice(&fs::read(root.join("ids.json")).unwrap()).unwrap();
        let disk = open(&root.join("Feature/project.fcmod"));
        assert_eq!(disk.project.item(master).unwrap().name == "Today's cut", stage == "save-other");
        let mut s = Session::local_folder_editor();
        s.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
        if stage == "discard" {
            assert!(s.recovery_candidates().is_empty());
            continue;
        }
        assert_eq!(s.recovery_candidates().len(), 1);
        let protected = !matches!(stage, "save-source" | "discard-source");
        assert_eq!(s.recovery_candidates()[0].meta.reviewed_source, protected.then_some(source));
        let chosen = root.join("Moved/camera.png");
        image::RgbImage::from_pixel(160, 90, image::Rgb([5u8, 188, 10])).save(&chosen).unwrap();
        s.execute("file.recover", json!({})).unwrap();
        assert!(s.project.sequence(deferred).is_some());
        assert_eq!(s.project.item(master).unwrap().name == "Today's cut", !matches!(stage, "warm" | "redo"));
        if protected {
            let media = s.project.item(source).unwrap().as_media().unwrap();
            assert!(media.offline);
            assert_eq!(media.media, filmcraft_project::MediaRef::File { path: chosen.to_string_lossy().into_owned() });
            let saved = root.join("recovered.fcproj");
            s.execute("file.saveAs", json!({"path":saved})).unwrap();
            assert!(open(&saved).project.item(source).unwrap().as_media().unwrap().offline);
        }
    }
}
