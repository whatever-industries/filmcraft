//! Exercise real Save, manifest replacement, OS locking and process termination on private fixtures.
use super::super::{index, tests::fixture};
use super::*;
use filmcraft_engine::Session;
use serde_json::json;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

fn editor(path: &Path) -> Result<Session> {
    let mut editor = Session::local_folder_editor();
    editor.execute("file.open", json!({"path":path}))?;
    Ok(editor)
}

#[test]
fn newer_save_during_refresh_wins_before_first_frame() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let data = temp.path().join("data");
    let old = Preview::open(&f.path, None)?;
    let mut editor = editor(&f.path)?;
    let mut staged = 0;
    let view = open_in(&f.path, &data, || {
        staged += 1;
        if staged == 1 {
            editor.execute("clip.rename", json!({"item":f.master,"name":"Published assembly"}))?;
            editor.edit("Switch camera", |p, _| {
                for track in p.sequence_mut(f.nested).unwrap().all_tracks_mut() {
                    track.items[0].item = f.other_camera;
                }
                Ok(())
            })?;
            editor.execute("file.save", json!({}))?;
        }
        Ok(())
    })?;
    let expected = Preview::open(&f.path, None)?;
    assert_eq!(view.image, expected.image);
    assert_ne!(view.image, old.image);
    assert_eq!(view.labels[&f.master].name, "Published assembly");
    assert_eq!(view.session.project.root, expected.session.project.root);
    let rebuilt = temp.path().join("manual.json");
    index(&f.path, &rebuilt)?;
    let cached = cache_directory(&f.path, &data)?.join("index.json");
    assert_eq!(fs::read(&cached)?, fs::read(&rebuilt)?);
    // Warm open must also work when a deferred payload is temporarily unavailable.
    let hashes: serde_json::Value = serde_json::from_slice(&fs::read(&f.path)?)?;
    let hash = hashes["items"].as_array().unwrap().iter().find(|item| item[0] == f.deferred.0).unwrap()[1].as_str().unwrap();
    let payload = f.path.with_file_name(hash);
    let aside = temp.path().join("unavailable");
    fs::rename(&payload, &aside)?;
    let warm = open_in(&f.path, &data, || Err("warm open rewrote the index".into()))?;
    assert_eq!(warm.image, expected.image);
    // An ordinary stale refresh can reuse that same deferred item's cached label.
    editor.execute("clip.rename", json!({"item":f.master,"name":"Latest assembly"}))?;
    // Save validates stored records, so restore the deferred file while the editor publishes.
    fs::rename(&aside, &payload)?;
    editor.execute("file.save", json!({}))?;
    fs::rename(&payload, &aside)?;
    let latest = open_in(&f.path, &data, || Ok(()))?;
    assert_eq!(latest.labels[&f.master].name, "Latest assembly");
    assert_eq!(view.labels[&f.master].name, "Published assembly");
    fs::rename(&aside, &payload)?;
    fs::write(&cached, b"truncated")?;
    let repaired = open_in(&f.path, &data, || Ok(()))?;
    assert_eq!(repaired.labels[&f.master].name, "Latest assembly");
    let rebuilt = temp.path().join("latest.json");
    index(&f.path, &rebuilt)?;
    assert_eq!(fs::read(cached)?, fs::read(rebuilt)?);
    Ok(())
}

#[test]
fn continuous_saves_stop_after_two_retries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let data = temp.path().join("data");
    let mut editor = editor(&f.path)?;
    let mut saves = 0;
    let result = open_in(&f.path, &data, || {
        saves += 1;
        editor.execute("clip.rename", json!({"item":f.master,"name":format!("Assembly {saves}")}))?;
        editor.execute("file.save", json!({}))?;
        Ok(())
    });
    assert!(result.err().unwrap().to_string().contains("kept changing"));
    assert_eq!(saves, 3);
    assert!(!cache_directory(&f.path, &data)?.join("index.json").exists());
    Ok(())
}

#[test]
fn interrupted_writer_leaves_only_its_abandoned_stage_for_reopen() -> Result<()> {
    const CHILD: &str = "FILMCRAFT_INDEX_CRASH_FIXTURE";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        open_in(&root.join("folder/project.fcmod"), &root.join("data"), || {
            println!("index stage synced");
            std::io::stdout().flush()?;
            loop {
                std::thread::park();
            }
        })?;
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let data = temp.path().join("data");
    open_in(&f.path, &data, || Ok(()))?;
    let dir = cache_directory(&f.path, &data)?;
    let dest = dir.join("index.json");
    let prior = fs::read(&dest)?;
    let mut editor = editor(&f.path)?;
    editor.execute("clip.rename", json!({"item":f.master,"name":"Saved before interruption"}))?;
    editor.execute("file.save", json!({}))?;
    let saved = fs::read(&f.path)?;
    let sentinel = dir.join("unrelated.tmp");
    fs::write(&sentinel, b"keep")?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "native::preview::cache::tests::interrupted_writer_leaves_only_its_abandoned_stage_for_reopen", "--nocapture"])
        .env(CHILD, temp.path())
        .stdout(Stdio::piped())
        .spawn()?;
    let ready = BufReader::new(child.stdout.take().unwrap()).lines().map_while(std::result::Result::ok).any(|line| line.contains("index stage synced"));
    let live = (|| -> Result<PathBuf> {
        if !ready {
            return Err("child exited before staging".into());
        }
        let stage = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.file_name().unwrap().as_encoded_bytes().starts_with(STAGE_PREFIX.as_bytes()))
            .ok_or("no stage")?;
        let busy = open_in(&f.path, &data, || Ok(())).err().ok_or("second preview acquired a live lock")?;
        if !busy.to_string().contains("another preview") || !stage.exists() {
            return Err("live stage was not protected".into());
        }
        Ok(stage)
    })();
    if ready {
        child.kill()?;
    }
    child.wait()?;
    let abandoned = live?;
    assert!(abandoned.exists());
    assert_eq!(fs::read(&dest)?, prior);
    let recovered = open_in(&f.path, &data, || Ok(()))?;
    assert_eq!(recovered.labels[&f.master].name, "Saved before interruption");
    assert!(!abandoned.exists());
    assert_eq!(fs::read(sentinel)?, b"keep");
    assert_eq!(fs::read(&f.path)?, saved);
    let rebuilt = temp.path().join("manual.json");
    index(&f.path, &rebuilt)?;
    assert_eq!(fs::read(dest)?, fs::read(rebuilt)?);
    Ok(())
}

#[test]
fn cache_root_inside_project_is_refused_before_creating_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let misplaced = f.path.with_file_name("data");
    assert!(open_in(&f.path, &misplaced, || Ok(())).is_err());
    assert!(!misplaced.exists());
    Ok(())
}
