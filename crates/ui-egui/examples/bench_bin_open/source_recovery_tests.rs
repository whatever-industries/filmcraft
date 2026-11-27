use super::*;

#[test]
fn force_quit_after_partial_relink_checkpoint_or_discard() -> Result<()> {
    use filmcraft_project::MediaRef;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    const CHILD: &str = "FILMCRAFT_RELINK_CRASH_CASE";
    let fresh = |data: &Path| -> Result<Session> {
        let mut s = Session::local_folder_editor();
        s.start_autosave(AutosaveConfig::new(data))?;
        Ok(s)
    };
    if let Ok(action) = std::env::var(CHILD) {
        let root = PathBuf::from(std::env::var_os("FILMCRAFT_RELINK_CRASH_DIR").unwrap());
        let data = root.join("data");
        if action == "reopen" {
            let review: Review = serde_json::from_slice(&fs::read(root.join("review.json"))?)?;
            let mut reopened = open(&root.join("recovered.fcproj"))?;
            reopened.state.active_sequence = reopened.project.sequences().find(|item| item.name == "Cut").map(|item| item.id);
            let item = reopened.project.item(review.item).unwrap();
            let media = item.as_media().unwrap();
            assert_eq!(item.name, "Renamed interview");
            assert_eq!(media.media, MediaRef::File { path: review.path });
            let offline = std::env::var("FILMCRAFT_EXPECT_OFFLINE")? == "true";
            assert_eq!(media.offline, offline);
            assert_eq!(picture(&reopened)? == fs::read(root.join("offline.rgba"))?, offline);
            assert!(!reopened.is_dirty());
            assert!(reopened.dirty_folder_bins().is_empty());
        } else if action == "recover" {
            let mut s = fresh(&data)?;
            s.execute("file.recover", json!({}))?;
            println!("relink acknowledged");
            std::thread::park();
        } else {
            let review: Review = serde_json::from_slice(&fs::read(root.join("review.json"))?)?;
            let mut edit = SourceEdit::open(&root.join("impact").join(project_folder::ROOT), &data, review)?;
            edit.action("apply")?;
            edit.view.session.execute("clip.rename", json!({"item":edit.candidate.item,"name":"Renamed interview"}))?;
            edit.action("checkpoint")?;
            assert!(edit.dirty());
            if action == "discard" {
                edit.action("discard")?;
                assert!(!edit.dirty());
            }
            println!("relink acknowledged");
            std::thread::park();
        }
        return Ok(());
    }

    let child_command = |root: &Path, action: &str| -> Result<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", "native::preview::source_edit::tests::crash::force_quit_after_partial_relink_checkpoint_or_discard", "--nocapture"])
            .env(CHILD, action)
            .env("FILMCRAFT_RELINK_CRASH_DIR", root);
        Ok(command)
    };
    let kill_after_ack = |root: &Path, action: &str| -> Result<()> {
        let mut child = child_command(root, action)?.stdout(Stdio::piped()).spawn()?;
        let acknowledged = BufReader::new(child.stdout.take().unwrap()).lines().map_while(std::result::Result::ok).any(|l| l.contains("relink acknowledged"));
        if acknowledged {
            child.kill()?;
        }
        child.wait()?;
        assert!(acknowledged, "child exited without acknowledging {action}");
        Ok(())
    };
    for case in ["unchanged", "missing", "different", "second crash", "discard"] {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        let f = cut(root, Case::Plain)?;
        let data = root.join("data");
        drop(warm(&f, &data)?);
        let manifest = fs::read(&f.manifest)?;
        let mut published = open(&f.manifest)?;
        published.state.active_sequence = Some(f.nested);
        let offline_picture = picture(&published)?;
        fs::write(root.join("offline.rgba"), &offline_picture)?;
        fs::write(root.join("review.json"), serde_json::to_vec(&f.review)?)?;
        kill_after_ack(root, if case == "discard" { "discard" } else { "checkpoint" })?;
        assert_eq!(fs::read(&f.manifest)?, manifest);
        let mut reopened = open(&f.manifest)?;
        reopened.state.active_sequence = Some(f.nested);
        assert_eq!(picture(&reopened)?, offline_picture);
        assert_ne!(reopened.project.item(f.review.item).unwrap().name, "Renamed interview");
        if case == "discard" {
            assert!(fresh(&data)?.recovery_candidates().is_empty());
            continue;
        }

        // A confirmed draft must remain independent of its original folder.
        fs::rename(f.manifest.parent().unwrap(), root.join("unmounted"))?;
        if case == "second crash" {
            kill_after_ack(root, "recover")?;
        }
        if case == "missing" {
            fs::rename(&f.review.path, root.join("moved-again.png"))?;
        } else if matches!(case, "different" | "second crash") {
            image::RgbImage::from_pixel(64, 32, image::Rgb([224u8, 0, 0])).save(&f.review.path)?;
        }
        let mut s = fresh(&data)?;
        assert_eq!(s.recovery_candidates().len(), 1);
        assert!(!s.recovery_candidates()[0].meta.clean_exit);
        s.execute("file.recover", json!({}))?;
        s.state.active_sequence = Some(f.nested);
        assert!(s.is_dirty());
        assert_eq!(s.dirty_folder_bins(), [f.review.bin].into());
        assert!(s.project.sequence(f.other).is_some());
        assert_eq!(s.project.sequence(f.other), published.project.sequence(f.other));
        assert_eq!(s.project.settings, published.project.settings);
        let working = s.project.item(f.review.item).unwrap();
        assert_eq!(working.name, "Renamed interview");
        let media = working.as_media().unwrap();
        assert_eq!(media.media, MediaRef::File { path: f.review.path.clone() });
        let offline = case != "unchanged";
        assert_eq!(media.offline, offline);
        assert_eq!(s.offline.missing.contains(&f.review.item), offline);
        assert_eq!(picture(&s)? == offline_picture, offline);
        if offline {
            assert!(s.offline.prompt);
        }
        let saved = root.join("recovered.fcproj");
        s.execute("file.saveAs", json!({"path":saved}))?;
        s.execute("file.save", json!({}))?;
        assert!(!s.is_dirty());
        assert!(s.dirty_folder_bins().is_empty());
        s.shutdown();
        if case == "missing" {
            // A different file can arrive after Save but before the next launch.
            image::RgbImage::from_pixel(64, 32, image::Rgb([224u8, 0, 0])).save(&f.review.path)?;
        }
        let result = child_command(root, "reopen")?.env("FILMCRAFT_EXPECT_OFFLINE", offline.to_string()).output()?;
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
    }
    Ok(())
}
