use super::*;
use crate::autosave::AutosaveConfig;
use filmcraft_project::{BinId, ItemId};
use std::{fs, path::PathBuf, time::Duration};

struct Fixture {
    manifest: PathBuf,
    cuts: BinId,
    selects: BinId,
    cut: ItemId,
    select: ItemId,
}

impl Fixture {
    fn new(dir: &Path) -> Self {
        let mut p = Project::new("Feature");
        let cuts = p.add_bin("Cuts", None);
        let selects = p.add_bin("Selects", Some(cuts));
        let cut = p.new_sequence("Cut", Default::default(), 1, 1, Some(cuts));
        let select = p.new_sequence("Select", Default::default(), 1, 1, Some(selects));
        let standard = dir.join("Feature.fcproj");
        fs::write(&standard, filmcraft_format::encode(&p, false)).unwrap();
        let folder = dir.join("Feature");
        filmcraft_format::project_folder::pack(&standard, &folder, filmcraft_format::atomic_write_new).unwrap();
        Self { manifest: folder.join("project.fcmod"), cuts, selects, cut, select }
    }

    fn open(&self) -> Session {
        let mut s = Session::local_folder_editor();
        s.execute("file.open", json!({"path":self.manifest})).unwrap();
        s
    }

    fn edit_both(&self, s: &mut Session) {
        trim(s, self.cut, 12);
        trim(s, self.select, 24);
        s.edit("Change project setting", |p, _| {
            p.settings.audio_display_samples = !p.settings.audio_display_samples;
            Ok(())
        })
        .unwrap();
    }
}

fn trim(s: &mut Session, id: ItemId, frame: i64) {
    s.edit("Change start timecode", |p, _| {
        p.sequence_mut(id).unwrap().start_timecode = frame;
        Ok(())
    })
    .unwrap();
}

fn frame(p: &Project, id: ItemId) -> i64 {
    p.sequence(id).unwrap().start_timecode
}

fn status(s: &mut Session) -> Value {
    s.execute("project.binStatus", json!({})).unwrap()
}

fn manifest(f: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(&f.manifest).unwrap()).unwrap()
}

fn record(m: &Value, group: &str, id: u64) -> Value {
    m[group].as_array().unwrap().iter().find(|pair| pair[0] == id).unwrap()[1].clone()
}

#[test]
fn saving_selects_publishes_only_its_edits_and_global_save_finishes_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    let before = manifest(&f);
    let baseline = s.project.clone();
    f.edit_both(&mut s);
    s.edit("Attach transcript", |p, _| {
        p.transcripts.insert(f.select, Default::default());
        Ok(())
    })
    .unwrap();
    let result = s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    assert!(result["bytes"].as_u64().unwrap() > 0);
    let after = manifest(&f);
    assert_eq!(before["bins"], after["bins"]);
    assert_eq!(record(&before, "items", f.cut.0), record(&after, "items", f.cut.0));
    assert_ne!(record(&before, "items", f.select.0), record(&after, "items", f.select.0));
    let disk = f.open();
    assert_eq!(frame(&disk.project, f.cut), frame(&baseline, f.cut));
    assert_eq!(frame(&disk.project, f.select), frame(&s.project, f.select));
    assert!(disk.project.transcripts.contains_key(&f.select));
    assert_eq!(disk.project.settings, baseline.settings);
    assert_eq!(status(&mut s), json!({"dirtyBins":[f.cuts],"projectSettingsDirty":true,"unfiledItemsDirty":false}));
    assert_eq!(s.execute("project.saveBin", json!({"bin":f.selects})).unwrap()["bytes"], 0);
    assert_eq!(manifest(&f), after);
    s.execute("file.save", json!({})).unwrap();
    assert_eq!(f.open().project, s.project);
    assert!(!s.is_dirty());
}

#[test]
fn parent_save_preserves_nested_edits_and_a_saved_bin_can_be_undone() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    trim(&mut s, f.select, 24);
    trim(&mut s, f.cut, 12);
    s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    assert_eq!(frame(&f.open().project, f.select), 0);
    assert_eq!(status(&mut s)["dirtyBins"], json!([f.selects]));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(status(&mut s)["dirtyBins"], json!([f.cuts, f.selects]));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(status(&mut s)["dirtyBins"], json!([f.selects]));
}

#[test]
fn discard_strips_a_bin_from_both_history_stacks() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    for (id, frame) in [(f.cut, 1), (f.select, 2), (f.cut, 3), (f.select, 4), (f.cut, 5)] {
        trim(&mut s, id, frame);
    }
    for _ in 0..2 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    let initial = manifest(&f);
    s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
    assert_eq!(manifest(&f), initial);
    assert_eq!(s.execute("project.discardBin", json!({"bin":f.selects})).unwrap()["discarded"], false);
    for (action, expected) in [("edit.redo", 5), ("edit.undo", 3), ("edit.undo", 1), ("edit.undo", 0), ("edit.redo", 1), ("edit.redo", 3), ("edit.redo", 5)] {
        s.execute(action, json!({})).unwrap();
        assert_eq!(frame(&s.project, f.cut), expected);
        assert_eq!(frame(&s.project, f.select), 0);
        assert_eq!(s.is_dirty(), expected != 0);
    }
}

fn worker(s: &mut Session, dir: &Path) {
    let mut cfg = AutosaveConfig::new(dir);
    cfg.journal_debounce = Duration::from_secs(60);
    s.start_autosave(cfg).unwrap();
}

#[test]
fn journal_failure_leaves_history_work_and_disk_intact_and_permits_retry() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    worker(&mut s, dir.path());
    f.edit_both(&mut s);
    s.sync_persistence();
    s.persistence.as_ref().unwrap().flush().unwrap();
    let journal = s.persistence.as_ref().unwrap().session_dir.clone();
    let marker: Value = serde_json::from_slice(&fs::read(journal.join("session.json")).unwrap()).unwrap();
    let manifest_path = journal.join("checkpoint-records").join(marker["checkpoint"].as_str().unwrap());
    let bytes = fs::read(&manifest_path).unwrap();
    let meta = journal.join("session.json");
    fs::rename(&meta, journal.join("old-meta")).unwrap();
    fs::create_dir(&meta).unwrap();
    let initial = manifest(&f);
    let history = s.history.undo.len();
    let work = s.project.clone();
    for command in ["project.saveBin", "project.discardBin"] {
        assert!(s.execute(command, json!({"bin":f.selects})).is_err());
        assert_eq!(s.project, work);
        assert_eq!(s.history.undo.len(), history);
        assert_eq!(manifest(&f), initial);
        assert_eq!(fs::read(&manifest_path).unwrap(), bytes);
    }
    fs::remove_dir(&meta).unwrap();
    s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    assert_eq!(frame(&f.open().project, f.select), 24);
    s.shutdown();
}

#[test]
fn unsupported_media_moves_and_stale_writers_leave_both_documents_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    let media = s.execute("file.newColorMatte", json!({"color":"#3377bb","width":160,"height":90,"seconds":2.0})).unwrap()["item"].as_u64().unwrap();
    s.execute("project.moveToBin", json!({"items":[media],"bin":f.cuts})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    let first = manifest(&f);
    s.edit("Move between bins", |p, _| {
        p.move_to_bin(&[ItemId(media)], Some(f.selects));
        Ok(())
    })
    .unwrap();
    let moved = s.project.clone();
    for bin in [f.cuts, f.selects] {
        for cmd in ["project.saveBin", "project.discardBin"] {
            assert!(s.execute(cmd, json!({"bin":bin})).unwrap_err().to_string().contains("moved between bins"));
            assert_eq!(s.project, moved);
            assert_eq!(manifest(&f), first);
        }
    }
    s.undo();
    let mut other = f.open();
    trim(&mut other, f.select, 2);
    other.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    trim(&mut s, f.cut, 3);
    let next = manifest(&f);
    for cmd in ["project.saveBin", "project.discardBin"] {
        assert!(s.execute(cmd, json!({"bin":f.cuts})).unwrap_err().to_string().contains("stale"));
        assert_eq!(frame(&s.project, f.cut), 3);
        assert_eq!(manifest(&f), next);
    }
}

#[test]
fn crash_after_bin_decision_recovers_only_the_remaining_work() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        thread,
    };
    const CHILD: &str = "FILMCRAFT_TEST_BIN_CRASH_DIR";
    if let Some(dir) = std::env::var_os(CHILD) {
        let dir = PathBuf::from(dir);
        let f = Fixture::new(&dir);
        let mut s = f.open();
        worker(&mut s, &dir);
        f.edit_both(&mut s);
        s.sync_persistence();
        s.persistence.as_ref().unwrap().flush().unwrap();
        let cmd = std::env::var("FILMCRAFT_TEST_BIN_DECISION").unwrap();
        if cmd == "both" || cmd == "cuts" {
            s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
        }
        match cmd.as_str() {
            "both" => {
                s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
            }
            "cuts" | "checkpoint" => {}
            _ => {
                s.execute(&cmd, json!({"bin":f.selects})).unwrap();
            }
        }
        println!("decision acknowledged");
        thread::park();
        return;
    }
    for cmd in ["checkpoint", "cuts", "both", "project.discardBin", "project.saveBin"] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "folder_edit::tests::crash_after_bin_decision_recovers_only_the_remaining_work", "--nocapture"])
            .env(CHILD, dir.path())
            .env("FILMCRAFT_TEST_BIN_DECISION", cmd)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let started =
            BufReader::new(child.stdout.take().unwrap()).lines().map_while(std::result::Result::ok).any(|line| line.contains("decision acknowledged"));
        if started {
            child.kill().unwrap();
        }
        let _ = child.wait().unwrap();
        assert!(started);
        let mut s = Session::local_folder_editor();
        worker(&mut s, dir.path());
        let live = FolderSnapshot::open(&dir.path().join("Feature/project.fcmod")).unwrap().1.project;
        let cut = live.sequences().find(|i| i.name == "Cut").unwrap().id;
        let select = live.sequences().find(|i| i.name == "Select").unwrap().id;
        fs::rename(dir.path().join("Feature"), dir.path().join("relocated")).unwrap();
        s.execute("file.recover", json!({})).unwrap();
        assert!(s.path.is_none() && s.is_dirty());
        assert_eq!(frame(&s.project, cut), 12);
        assert_eq!(frame(&live, cut), if matches!(cmd, "cuts" | "both") { 12 } else { 0 });
        assert_eq!(frame(&s.project, select), if matches!(cmd, "both" | "project.discardBin") { 0 } else { 24 });
        assert_eq!(frame(&live, select), if cmd == "project.saveBin" { 24 } else { 0 });
        assert_ne!(live.settings.audio_display_samples, s.project.settings.audio_display_samples);
        s.shutdown();
    }
}

#[test]
fn save_and_discard_require_all_referenced_items_to_survive() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    // Selects depends on a new source in Cuts. The merged clip first becomes
    // independently saveable once its source has been published.
    let source = s
        .edit("Make linked source", |p, _| {
            let source = p.new_sequence("Source", Default::default(), 1, 0, Some(f.cuts));
            p.sequence_mut(f.select).unwrap().merged = Some(filmcraft_project::MergedClip { video: Some(source), ..Default::default() });
            Ok(source)
        })
        .unwrap();
    let initial = manifest(&f);
    let working = s.project.clone();
    for (cmd, bin) in [("project.saveBin", f.selects), ("project.discardBin", f.cuts)] {
        assert!(s.execute(cmd, json!({"bin":bin})).unwrap_err().to_string().contains("required project item"));
        assert_eq!(manifest(&f), initial);
        assert_eq!(s.project, working);
    }
    s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    assert!(f.open().project.sequence(f.select).unwrap().merged.is_none());
    s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    assert_eq!(f.open().project.sequence(f.select).unwrap().merged.as_ref().unwrap().video, Some(source));
    assert!(!s.is_dirty());
}

#[test]
fn blocked_publication_and_failed_postpublication_cleanup_are_distinguishable() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    worker(&mut s, dir.path());
    trim(&mut s, f.select, 24);
    let before = manifest(&f);
    let archive = f.manifest.with_file_name(s.folder().unwrap().snapshot.revision());
    fs::create_dir(&archive).unwrap();
    assert!(s.execute("project.saveBin", json!({"bin":f.selects})).unwrap_err().to_string().contains("bin was not saved"));
    assert_eq!(manifest(&f), before);
    assert_eq!(status(&mut s)["dirtyBins"], json!([f.selects]));
    fs::remove_dir(archive).unwrap();
    // A non-file at the candidate path makes even clearing the journal fail.
    let snapshot = s.persistence.as_ref().unwrap().session_dir.join("session.json");
    fs::create_dir(&snapshot).unwrap();
    let error = s.execute("project.saveBin", json!({"bin":f.selects})).unwrap_err().to_string();
    assert!(error.contains("was published") && error.contains("cleanup failed"), "{error}");
    let published = manifest(&f);
    assert_ne!(published, before);
    assert_eq!(frame(&f.open().project, f.select), 24);
    fs::remove_dir(snapshot).unwrap();
    assert_eq!(s.execute("project.saveBin", json!({"bin":f.selects})).unwrap()["bytes"], 0);
    assert_eq!(manifest(&f), published);
    s.shutdown();
}

#[test]
fn disabled_recovery_respects_bin_decisions_without_a_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    worker(&mut s, dir.path());
    s.execute("prefs.set", json!({"key":"autoSave.recoveryJournal","value":false})).unwrap();
    f.edit_both(&mut s);
    s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
    assert_eq!(status(&mut s)["projectSettingsDirty"], true);
    let path = s.persistence.as_ref().unwrap().session_dir.join("session.json");
    assert!(!path.exists());
    s.shutdown();
    assert!(crate::autosave::scan_candidates(dir.path(), None).is_empty());
}

#[test]
fn discarding_the_only_dirty_bin_removes_the_old_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    worker(&mut s, dir.path());
    let baseline = s.project.clone();
    let created = s.edit("Create sequence in Selects", |p, _| Ok(p.new_sequence("Alternate", Default::default(), 1, 1, Some(f.selects)))).unwrap();
    s.sync_persistence();
    s.persistence.as_ref().unwrap().flush().unwrap();
    let journal = s.persistence.as_ref().unwrap().session_dir.join("session.json");
    assert!(journal.is_file());
    s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
    assert!(!journal.exists());
    assert!(s.project.item(created).is_none());
    assert_eq!(s.project.root, baseline.root);
    assert!(!s.is_dirty() && s.history.undo.is_empty());
    s.shutdown();
}

#[test]
fn damaged_record_blocks_bin_decisions_and_standard_projects_disable_them() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    trim(&mut s, f.select, 8);
    let published = manifest(&f);
    let hash = record(&published, "items", f.cut.0);
    fs::write(f.manifest.with_file_name(hash.as_str().unwrap()), b"broken").unwrap();
    for cmd in ["project.saveBin", "project.discardBin"] {
        assert!(s.execute(cmd, json!({"bin":f.selects})).unwrap_err().to_string().contains("damaged"));
        assert_eq!(frame(&s.project, f.select), 8);
        assert_eq!(manifest(&f), published);
    }
    s.execute("file.open", json!({"path":dir.path().join("Feature.fcproj")})).unwrap();
    for cmd in ["project.binStatus", "project.saveBin", "project.discardBin", "project.restoreSequenceCopy", "project.exportRecoverySequence"] {
        assert!(!s.is_enabled(cmd));
    }
}

#[test]
fn save_removal_protects_departure_version_and_discard_preserves_the_saved_cut() {
    for discard_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let f = Fixture::new(dir.path());
        let mut s = f.open();
        let original = s.folder_revision().unwrap().to_owned();
        trim(&mut s, f.cut, 2);
        s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.selects})).unwrap();
        trim(&mut s, f.cut, 3);
        if discard_first {
            s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
            assert_eq!(status(&mut s)["dirtyBins"], json!([f.cuts]));
            assert_eq!(s.project.root.parent_of(f.cut), None);
        }
        let blocker = f.manifest.with_file_name(&original);
        fs::create_dir(&blocker).unwrap();
        assert!(s.execute("project.saveBin", json!({"bin":f.cuts})).is_err());
        assert_eq!(s.folder_revision().unwrap(), original);
        assert!(s.bin_has_move_removal(f.cuts));
        fs::remove_dir(blocker).unwrap();
        s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
        if !discard_first {
            s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
        }
        assert_eq!(s.project.root.parent_of(f.cut), None);
        assert!(!s.is_dirty());
        let folder = f.manifest.parent().unwrap();
        let mut backup = None;
        filmcraft_format::project_folder::move_backups(folder, |_, b| {
            backup = Some(b.snapshot.clone());
            Ok(())
        })
        .unwrap();
        let backup = backup.unwrap();
        let versions = s.execute("project.recoveryVersions", json!({})).unwrap();
        assert!(
            versions["versions"].as_array().unwrap().iter().any(|v| v["moves"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["snapshot"] == backup && m["sequence"] == f.cut.0))
        );
        let restored = s.execute("project.restoreSequenceCopy", json!({"revision":backup,"sequence":f.cut})).unwrap();
        let recovered = ItemId(restored["item"].as_u64().unwrap());
        assert_eq!(frame(&s.project, recovered), 2);
        assert_eq!(s.project.root.parent_of(recovered), Some(s.project.root.id));
        s.execute("edit.undo", json!({})).unwrap();
        let version1 = dir.path().join("original.fcproj");
        filmcraft_format::project_folder::rescue(folder, &original, f.cut.0, &version1).unwrap();
        assert_eq!(frame(&filmcraft_format::decode(&fs::read(version1).unwrap()).unwrap().project, f.cut), 0);
        trim(&mut s, f.select, 17);
        s.execute("file.save", json!({})).unwrap();
        let (copy, _) = filmcraft_format::project_folder::restore_copy(folder, s.folder_revision().unwrap(), &backup, f.cut.0).unwrap();
        let disk = f.open();
        assert_eq!(frame(&disk.project, ItemId(copy)), 2);
        assert_eq!(frame(&disk.project, f.cut), 0);
        assert_eq!(frame(&disk.project, f.select), 17);
        assert_eq!(disk.project.root.parent_of(f.cut), None);
    }
}

#[test]
fn source_saved_after_destination_retires_the_surviving_copy_only() {
    for global in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let f = Fixture::new(dir.path());
        let mut s = f.open();
        let original = manifest(&f);
        s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.selects})).unwrap();
        trim(&mut s, f.cut, 3);
        s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
        assert_eq!(record(&manifest(&f), "bins", f.cuts.0), record(&original, "bins", f.cuts.0));
        assert_eq!(status(&mut s)["dirtyBins"], json!([f.cuts]));
        if global {
            s.execute("file.save", json!({})).unwrap();
        } else {
            s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
        }
        let disk = f.open();
        assert_eq!(disk.project.root.parent_of(f.cut), Some(f.selects));
        assert_eq!(disk.project.items.len(), s.project.items.len());
        assert_eq!(frame(&disk.project, f.cut), 3);
        assert_eq!(s.execute("file.save", json!({})).unwrap()["bytes"], 0);
    }
}

#[test]
fn corrupt_departure_backup_blocks_source_removal_and_retry_keeps_the_original_destination() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    trim(&mut s, f.cut, 2);
    s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.selects})).unwrap();
    trim(&mut s, f.cut, 3);
    s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    let published = manifest(&f);
    let folder = f.manifest.parent().unwrap();
    let moves = &published["revision"]["moves"];
    let archive = folder.join(moves[0]["snapshot"].as_str().unwrap());
    let saved = fs::read(&archive).unwrap();
    fs::write(&archive, b"damaged").unwrap();
    assert!(s.execute("project.saveBin", json!({"bin":f.cuts})).is_err());
    assert_eq!(manifest(&f), published);
    assert!(s.dirty_folder_bins().contains(&f.cuts));
    fs::write(archive, saved).unwrap();
    worker(&mut s, dir.path());
    s.sync_persistence();
    let persistence = s.persistence.as_ref().unwrap();
    persistence.flush().unwrap();
    let file = persistence.session_dir.join("session.json");
    fs::rename(&file, file.with_extension("previous")).unwrap();
    fs::create_dir(&file).unwrap();
    let err = s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap_err();
    assert!(matches!(err, EngineError::BinPublished { .. }));
    let after = manifest(&f);
    fs::remove_dir(file).unwrap();
    let retry = s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    assert_eq!(retry["bytes"], 0);
    assert_eq!(manifest(&f), after);
    let live = f.open();
    assert_eq!(frame(&live.project, f.cut), 3);
    assert_eq!(live.project.root.parent_of(f.cut), Some(f.selects));
    s.shutdown();
}

#[test]
fn a_surviving_copy_can_move_and_be_edited_without_changing_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    let bin_c = s.edit("Add Bin C", |p, _| Ok(p.add_bin("Bin C", None))).unwrap();
    s.execute("file.save", json!({})).unwrap();
    let before = manifest(&f);
    s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.selects})).unwrap();
    trim(&mut s, f.cut, 4);
    s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    let mut fresh = f.open();
    let copy = fresh.project.sequences().find(|i| fresh.project.root.parent_of(i.id) == Some(f.cuts)).unwrap().id;
    fresh.execute("project.moveToBin", json!({"items":[copy],"bin":bin_c})).unwrap();
    trim(&mut fresh, copy, 7);
    fresh.execute("project.saveBin", json!({"bin":bin_c})).unwrap();
    assert_eq!(record(&manifest(&f), "bins", f.cuts.0), record(&before, "bins", f.cuts.0));
    let mut disk = f.open();
    let survivor = disk.project.sequences().find(|i| disk.project.root.parent_of(i.id) == Some(f.cuts)).unwrap().id;
    assert_ne!(survivor, copy);
    assert_eq!(frame(&disk.project, survivor), 0);
    trim(&mut disk, survivor, 9);
    disk.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    let verify = f.open();
    assert_eq!(frame(&verify.project, survivor), 9);
    assert_eq!(frame(&verify.project, copy), 7);
    assert_eq!(frame(&verify.project, f.cut), 4);
}

#[test]
fn discard_arrival_then_removal_restores_original_membership_and_other_edits() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.selects})).unwrap();
    trim(&mut s, f.cut, 3);
    s.execute("project.discardBin", json!({"bin":f.selects})).unwrap();
    assert_eq!(s.project.root.parent_of(f.cut), None);
    assert_eq!(status(&mut s)["dirtyBins"], json!([f.cuts]));
    trim(&mut s, f.select, 17);
    s.execute("project.discardBin", json!({"bin":f.cuts})).unwrap();
    assert_eq!(s.project.root.parent_of(f.cut), Some(f.cuts));
    assert_eq!(frame(&s.project, f.cut), 0);
    assert_eq!(frame(&s.project, f.select), 17);
    assert_eq!(status(&mut s)["dirtyBins"], json!([f.selects]));
}

#[test]
fn retention_counts_published_bins_across_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    let defaults = s.execute("project.recoveryRetention", json!({})).unwrap();
    assert_eq!(defaults["enabled"], true);
    let mut policy = json!({"enabled":false,"maxBinBackups":2,"versionsPerBin":2});
    s.execute("project.setRecoveryRetention", policy.clone()).unwrap();
    let base = s.folder_revision().unwrap().to_owned();
    let preview = |s: &mut Session| s.execute("project.previewRecoveryPrune", json!({})).unwrap();
    f.edit_both(&mut s);
    s.execute("file.save", json!({})).unwrap();
    assert_eq!(preview(&mut s)["binBackups"], 2);
    s.execute("file.save", json!({})).unwrap();
    s.execute("file.projectSettings.general", json!({"titleSafe":[11,13]})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    assert_eq!(preview(&mut s)["binBackups"], 2);
    trim(&mut s, f.cut, 33);
    s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    let mut reopened = f.open();
    assert_eq!(reopened.execute("project.recoveryRetention", json!({})).unwrap(), policy);
    assert_eq!(preview(&mut reopened)["binBackups"], 3);
    policy["enabled"] = json!(true);
    reopened.execute("project.setRecoveryRetention", policy).unwrap();
    let before = preview(&mut reopened);
    assert_eq!(before["due"], true);
    trim(&mut reopened, f.select, 45);
    let result = reopened.execute("file.save", json!({})).unwrap();
    assert!(result["retentionWarning"].is_null(), "{result}");
    reopened.execute("project.waitForRecoveryPrune", json!({})).unwrap();
    assert_eq!(preview(&mut reopened)["due"], false);
    let mut versions = Vec::new();
    filmcraft_format::project_folder::history(f.manifest.parent().unwrap(), |_, hash| {
        versions.push(hash.to_owned());
        Ok(())
    })
    .unwrap();
    assert!(!versions.contains(&base));
    assert_eq!(frame(&f.open().project, f.cut), 33);
    assert_eq!(frame(&f.open().project, f.select), 45);
}

#[test]
fn unrelated_bin_cleanup_keeps_the_source_of_an_unpublished_move() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    let third = s.edit("Add a third bin", |p, _| Ok(p.add_bin("Footage", None))).unwrap();
    s.execute("file.save", json!({})).unwrap();
    let source = s.folder_revision().unwrap().to_owned();
    s.execute("project.setRecoveryRetention", json!({"enabled":true,"maxBinBackups":1,"versionsPerBin":1})).unwrap();
    trim(&mut s, f.cut, 2);
    s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.selects})).unwrap();
    for name in ["Day 1", "Day 2", "Day 3"] {
        s.execute("project.renameBin", json!({"bin":third,"name":name})).unwrap();
        let saved = s.execute("project.saveBin", json!({"bin":third})).unwrap();
        assert!(saved["retentionWarning"].is_null(), "{saved}");
    }
    let saved = s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    assert!(saved["retentionWarning"].is_null(), "{saved}");
    s.execute("project.waitForRecoveryPrune", json!({})).unwrap();
    let folder = f.manifest.parent().unwrap();
    let mut backups = Vec::new();
    filmcraft_format::project_folder::move_backups(folder, |_, b| {
        backups.push(b.clone());
        Ok(())
    })
    .unwrap();
    assert!(backups.iter().any(|b| b.saved_source == source && b.sequence == f.cut.0));
    let rescued = dir.path().join("departure.fcproj");
    filmcraft_format::project_folder::rescue(folder, &backups[0].snapshot, f.cut.0, &rescued).unwrap();
    let cut = filmcraft_format::decode(&fs::read(rescued).unwrap()).unwrap();
    assert_eq!(frame(&cut.project, f.cut), 2);
}

#[test]
fn an_editor_can_save_and_pin_while_pruning_waits_and_reopen_finishes_the_work() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let folder = f.manifest.parent().unwrap();
    // Pause the planner, without holding the project writer.
    let planner = fs::File::create(folder.join(".retention-worker.lock")).unwrap();
    planner.lock().unwrap();
    let mut s = f.open();
    let base = s.folder_revision().unwrap().to_owned();
    let mut policy = json!({"enabled":true,"maxBinBackups":1,"versionsPerBin":1});
    s.execute("project.setRecoveryRetention", policy.clone()).unwrap();
    for n in 1..=3 {
        trim(&mut s, f.cut, n);
        s.execute("project.saveBin", json!({"bin":f.cuts})).unwrap();
    }
    s.execute("project.keepRecoveryVersion", json!({"revision":base,"keep":true})).unwrap();
    policy["enabled"] = json!(false);
    s.execute("project.setRecoveryRetention", policy.clone()).unwrap();
    drop(planner);
    s.execute("project.waitForRecoveryPrune", json!({})).unwrap();
    assert_eq!(s.execute("project.previewRecoveryPrune", json!({})).unwrap()["binBackups"], 3);
    policy["enabled"] = json!(true);
    s.execute("project.setRecoveryRetention", policy).unwrap();
    drop(s);
    let mut reopened = f.open();
    reopened.execute("project.waitForRecoveryPrune", json!({})).unwrap();
    assert_eq!(reopened.execute("project.previewRecoveryPrune", json!({})).unwrap()["binBackups"], 1);
    let mut retained = Vec::new();
    filmcraft_format::project_folder::history(folder, |_, hash| {
        retained.push(hash.to_owned());
        Ok(())
    })
    .unwrap();
    assert!(retained.contains(&base));
    let (copy, _) = filmcraft_format::project_folder::restore_copy(folder, reopened.folder_revision().unwrap(), &base, f.cut.0).unwrap();
    let restored = f.open();
    assert_eq!(frame(&restored.project, f.cut), 3);
    assert_eq!(frame(&restored.project, ItemId(copy)), 0);
}

#[test]
fn browsing_older_pages_and_a_retired_selection_preserve_the_edit() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::new(dir.path());
    let mut s = f.open();
    let original = s.folder_revision().unwrap().to_owned();
    for n in 1..=15 {
        trim(&mut s, f.cut, n);
        s.execute("file.save", json!({})).unwrap();
    }
    let page = s.execute("project.recoveryVersions", json!({})).unwrap();
    let older = s.execute("project.recoveryVersions", json!({"before":page["before"]})).unwrap();
    let numbers: Vec<_> = [page, older].iter().flat_map(|p| p["versions"].as_array().unwrap()).map(|v| v["number"].as_u64().unwrap()).collect();
    assert_eq!(numbers, (0..=15).rev().collect::<Vec<_>>());
    s.execute("project.setRecoveryRetention", json!({"enabled":true,"maxBinBackups":1,"versionsPerBin":1})).unwrap();
    trim(&mut s, f.cut, 16);
    s.execute("file.save", json!({})).unwrap();
    s.execute("project.waitForRecoveryPrune", json!({})).unwrap();
    trim(&mut s, f.cut, 21);
    let before = s.project.clone();
    assert!(s.execute("project.restoreSequenceCopy", json!({"revision":original,"sequence":f.cut})).is_err());
    assert_eq!(s.project, before);
}

#[test]
fn folder_moves_publish_with_global_save_and_export_as_an_ordinary_tree() {
    use filmcraft_project::DirectoryEntry::{Bin, Folder};
    let tmp = tempfile::tempdir().unwrap();
    let f = Fixture::new(tmp.path());
    let project_files = || {
        fs::read_dir(f.manifest.parent().unwrap())
            .unwrap()
            .map(|e| {
                let entry = e.unwrap();
                let stat = entry.metadata().unwrap();
                (entry.file_name(), (stat.len(), stat.modified().unwrap()))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let original_files = project_files();
    let before = manifest(&f);
    let mut s = f.open();
    let root = s.project.root.id;
    let old = s.project.directory();
    assert_eq!(old.parent(Bin(f.cuts)), Some(f.cuts));
    assert_eq!(old.parent(Folder(f.cuts)), Some(root));
    assert_eq!(old.parent(Bin(f.selects)), Some(f.cuts));
    assert_eq!(manifest(&f), before);
    assert_eq!(project_files(), original_files);
    let dest = BinId(s.execute("file.newFolder", json!({"name":"Finishing"})).unwrap()["folder"].as_u64().unwrap());
    s.execute("project.moveToFolder", json!({"folder":dest,"entry":Bin(f.selects)})).unwrap();
    assert!(s.execute("project.moveToFolder", json!({"folder":dest,"entry":Folder(dest)})).is_err());
    let pending = status(&mut s);
    assert_eq!(pending["dirtyBins"], json!([]));
    assert_eq!(pending["projectSettingsDirty"], false);
    assert_eq!(pending["projectDirectoryDirty"], true);
    trim(&mut s, f.select, 12);
    s.execute("project.saveBin", json!({"bin":f.selects})).unwrap();
    let reopened = f.open();
    assert_eq!(frame(&reopened.project, f.select), 12);
    assert_eq!(reopened.project.directory().parent(Bin(f.selects)), Some(f.cuts));
    let bin_save = manifest(&f);
    s.execute("file.save", json!({})).unwrap();
    let after = manifest(&f);
    for key in ["bins", "items"] {
        assert_eq!(bin_save[key], after[key]);
    }
    assert_eq!(f.open().project.directory().parent(Bin(f.selects)), Some(dest));
    let exported = tmp.path().join("portable.fcproj");
    filmcraft_format::project_folder::unpack(f.manifest.parent().unwrap(), &exported).unwrap();
    let p = filmcraft_format::decode(&fs::read(exported).unwrap()).unwrap().project;
    assert!(p.modular_directory.is_none());
    let parent = p.root.find_bin(dest).unwrap();
    assert!(parent.find_bin(f.selects).is_some());
    assert_eq!(p.root.parent_of(f.cut), Some(f.cuts));
    assert_eq!(p.root.parent_of(f.select), Some(f.selects));
    assert_eq!(frame(&p, f.select), 12);
}

#[test]
fn first_selective_save_publishes_only_the_required_new_folder_path() {
    use filmcraft_project::DirectoryEntry::{Bin, Folder};
    let tmp = tempfile::tempdir().unwrap();
    let f = Fixture::new(tmp.path());
    let mut s = f.open();
    let a = s.execute("file.newFolder", json!({"name":"Show"})).unwrap()["folder"].clone();
    let b = s.execute("file.newFolder", json!({"name":"Episode","parent":a})).unwrap()["folder"].clone();
    let unused = s.execute("file.newFolder", json!({"name":"Unused","parent":a})).unwrap()["folder"].clone();
    let bin = s.execute("file.newBin", json!({"name":"Assembly","parent":b})).unwrap()["bin"].clone();
    let other = s.execute("file.newBin", json!({"name":"Other","parent":b})).unwrap()["bin"].clone();
    s.execute("project.saveBin", json!({"bin":bin})).unwrap();
    let reopened = f.open();
    let dir = reopened.project.directory();
    let id = |v: &Value| BinId(v.as_u64().unwrap());
    assert_eq!(dir.parent(Bin(id(&bin))), Some(id(&b)));
    assert_eq!(dir.parent(Folder(id(&b))), Some(id(&a)));
    assert!(dir.parent(Bin(id(&other))).is_none());
    assert!(!dir.folders.contains_key(&id(&unused)));
    assert!(s.is_dirty());
}

#[test]
fn organizing_an_indexed_project_keeps_reels_deferred_and_checkpoint_independent() {
    use filmcraft_format::project_folder::{
        FolderDirectory,
        index::{Index, cache_directory},
        load_checkpoint,
    };
    use filmcraft_project::DirectoryEntry::{Bin, Folder};
    let scratch = tempfile::tempdir().unwrap();
    let manifest = if let Some(source) = std::env::var_os("FILMCRAFT_FOLDER_TRIAL") {
        let folder = scratch.path().join("Feature");
        fs::create_dir(&folder).unwrap();
        for file in fs::read_dir(source).unwrap().map(|e| e.unwrap()) {
            if file.file_type().unwrap().is_file() {
                fs::copy(file.path(), folder.join(file.file_name())).unwrap();
            }
        }
        folder.join("project.fcmod")
    } else {
        let fixture = Fixture::new(scratch.path());
        let session = fixture.open();
        let src = scratch.path().join("view.fcproj");
        let view = filmcraft_project::ProjectView { active_sequence: Some(fixture.cut), ..Default::default() };
        fs::write(&src, filmcraft_format::encode_with_view(&session.project, Some(&view), false)).unwrap();
        let folder = scratch.path().join("WithView");
        filmcraft_format::project_folder::pack(&src, &folder, filmcraft_format::atomic_write_new).unwrap();
        folder.join("project.fcmod")
    };
    let cache = scratch.path().join("cache");
    let d = FolderDirectory::open(&manifest).unwrap();
    let (index, _, _) = Index::build(&d, None).unwrap();
    fs::write(cache_directory(&manifest, &cache).unwrap().join("index.json"), index.bytes().unwrap()).unwrap();
    let total = index.labels.values().filter(|l| l.sequence).count();
    let mut s = Session::local_folder_editor();
    s.enable_experimental_modular_relink(cache);
    s.execute("file.open", json!({"path":manifest})).unwrap();
    worker(&mut s, scratch.path());
    let resident = |s: &Session| s.project.sequences().map(|q| q.id).collect::<std::collections::BTreeSet<_>>();
    let loaded = resident(&s);
    assert!(s.has_deferred_timelines() && loaded.len() < total);
    let before: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let dir = s.project.directory().into_owned();
    let moving = *dir.folders.values().flat_map(|f| &f.children).find(|e| matches!(e, Bin(_))).unwrap();
    let new = BinId(s.execute("file.newFolder", json!({"name":"Finishing"})).unwrap()["folder"].as_u64().unwrap());
    s.execute("project.moveToFolder", json!({"entry":moving,"folder":new})).unwrap();
    s.execute("project.directory", json!({})).unwrap();
    assert_eq!(resident(&s), loaded);
    s.sync_persistence();
    let worker = s.persistence.as_ref().unwrap();
    worker.flush().unwrap();
    let journal = worker.session_dir.clone();
    let meta: Value = serde_json::from_slice(&fs::read(journal.join("session.json")).unwrap()).unwrap();
    let marker = meta["checkpoint"].as_str().unwrap();
    // Prevent publication after the checkpoint was confirmed, as if interrupted before root replacement.
    let archive = manifest.with_file_name(d.revision);
    fs::create_dir(&archive).unwrap();
    assert!(s.execute("file.save", json!({})).is_err());
    let source = manifest.parent().unwrap();
    let detached = scratch.path().join("unmounted");
    fs::rename(source, &detached).unwrap();
    let recovered = load_checkpoint(&journal, marker).unwrap().project;
    assert_eq!(recovered.directory().parent(moving), Some(new));
    assert_eq!(recovered.sequences().count(), total);
    fs::rename(&detached, source).unwrap();
    fs::remove_dir(archive).unwrap();
    let result = s.execute("file.save", json!({})).unwrap();
    assert!(s.has_deferred_timelines());
    assert_eq!(resident(&s), loaded);
    let after: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    for k in ["items", "bins"] {
        assert_eq!(after[k], before[k]);
    }
    assert_eq!(after["revision"]["total_bin_backups"].as_u64().unwrap_or(0), before["revision"]["total_bin_backups"].as_u64().unwrap_or(0));
    let reopened = FolderSnapshot::open(&manifest).unwrap().1.project;
    assert_eq!(reopened.directory().parent(Folder(new)), Some(dir.root));
    assert_eq!(reopened.directory().parent(moving), Some(new));
    eprintln!("folder trial: {total} sequences, {} resident, {} bytes published", loaded.len(), result["bytes"]);
    s.shutdown();
}

#[test]
fn retained_bin_media_check_follows_cross_bin_sources_and_includes_unused_clips() {
    use filmcraft_project::{MediaRef, MergedClip};
    let tmp = tempfile::tempdir().unwrap();
    let f = Fixture::new(tmp.path());
    let mut s = f.open();
    let path = f.manifest.with_file_name("original.mov");
    let proxy = f.manifest.with_file_name("proxy.mov");
    fs::write(&path, b"original footage").unwrap();
    fs::write(&proxy, b"proxy footage").unwrap();
    let mut ids = Vec::new();
    for bin in [f.cuts, f.selects, f.selects] {
        let result = s.execute("file.newColorMatte", json!({"color":"#2288bb","bin":bin})).unwrap();
        let id = ItemId(result["item"].as_u64().unwrap());
        ids.push(id);
        s.edit("Attach test media", |p, _| {
            let media = p.item_mut(id).unwrap().as_media_mut().unwrap();
            media.media = MediaRef::File { path: "original.mov".into() };
            media.proxy = Some(MediaRef::File { path: "proxy.mov".into() });
            media.identity = Some(crate::relink::identity_of_bytes(b"original footage"));
            Ok(())
        })
        .unwrap();
    }
    s.edit("Reference another bin", |p, _| {
        p.sequence_mut(f.cut).unwrap().merged = Some(MergedClip { video: Some(ids[1]), ..Default::default() });
        Ok(())
    })
    .unwrap();
    s.execute("file.save", json!({})).unwrap();
    let saved = s.folder_revision().unwrap().to_owned();
    trim(&mut s, f.cut, 24);
    s.execute("file.save", json!({})).unwrap();
    let backup = tmp.path().join("Backup");
    filmcraft_format::project_folder::backup_history(f.manifest.parent().unwrap(), &backup).unwrap();
    fs::copy(&proxy, backup.join("proxy.mov")).unwrap();
    fs::rename(f.manifest.parent().unwrap(), tmp.path().join("Detached")).unwrap();
    let mut restored = Session::local_folder_editor();
    let backup_root = backup.join("project.fcmod");
    restored.execute("file.open", json!({"path":backup_root})).unwrap();
    let head = fs::read(&backup_root).unwrap();
    for (contents, state) in [(None, "missing"), (Some(b"replacement".as_slice()), "changed"), (Some(b"original footage".as_slice()), "matched")] {
        if let Some(bytes) = contents {
            fs::write(backup.join("original.mov"), bytes).unwrap()
        } else {
            assert!(!backup.join("original.mov").exists());
        }
        let result = restored.execute("project.inspectBinMedia", json!({"bin":f.cuts,"revision":saved})).unwrap();
        let checked: std::collections::BTreeSet<ItemId> = serde_json::from_value(result["inspectedItems"].clone()).unwrap();
        assert_eq!(checked, [f.cut, ids[0], ids[1]].into_iter().collect());
        let files = result["media"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        for file in files {
            assert_eq!(file["original"], state);
            assert_eq!(file["proxy"], "presentUnverified");
        }
        assert_eq!(fs::read(&backup_root).unwrap(), head);
        assert!(!restored.is_dirty());
    }
}

#[test]
fn folder_export_keeps_extension_fields_saved_views_and_unfiled_items() {
    let tmp = tempfile::tempdir().unwrap();
    let mut p = Project::new("Reels");
    let empty = p.add_bin("Same", None);
    let mixed = p.add_bin("Same", None);
    let first = p.new_sequence("Opening", Default::default(), 1, 0, Some(mixed));
    let leaf = p.add_bin("Selects", Some(mixed));
    let second = p.new_sequence("Master", Default::default(), 1, 0, Some(mixed));
    p.sequence_mut(second).unwrap().merged = Some(filmcraft_project::MergedClip { video: Some(first), ..Default::default() });
    let root_item = p.new_sequence("Top", Default::default(), 1, 0, None);
    let unfiled = p.new_sequence("Unfiled", Default::default(), 1, 0, None);
    p.root.remove_item(unfiled);
    let view = filmcraft_project::ProjectView { open_sequences: vec![first, second], active_sequence: Some(second), ..Default::default() };
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode_with_view(&p, Some(&view), false)).unwrap();
    doc["project"]["items"][first.0.to_string()]["extensionExample"] = json!({"v":[1,8]});
    doc["extensionExample"] = json!(["kept"]);
    let src = tmp.path().join("source.fcproj");
    fs::write(&src, serde_json::to_vec(&doc).unwrap()).unwrap();
    let folder = tmp.path().join("folder");
    filmcraft_format::project_folder::pack(&src, &folder, filmcraft_format::atomic_write_new).unwrap();
    let portable = tmp.path().join("clean.fcproj");
    filmcraft_format::project_folder::unpack(&folder, &portable).unwrap();
    let clean: Value = serde_json::from_slice(&fs::read(portable).unwrap()).unwrap();
    assert_eq!(clean, doc);
    let mut s = Session::local_folder_editor();
    s.execute("file.open", json!({"path":folder.join("project.fcmod")})).unwrap();
    let to = s.execute("file.newFolder", json!({"name":"Finishing"})).unwrap()["folder"].clone();
    s.execute("project.moveToFolder", json!({"folder":to,"entry":{"Bin":leaf}})).unwrap();
    let output = tmp.path().join("edited.fcproj");
    s.execute("file.saveCopy", json!({"path":output})).unwrap();
    let data = fs::read(output).unwrap();
    let edited: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(edited["extensionExample"], doc["extensionExample"]);
    assert_eq!(edited["project"]["items"][first.0.to_string()]["extensionExample"], doc["project"]["items"][first.0.to_string()]["extensionExample"]);
    let loaded = filmcraft_format::decode(&data).unwrap();
    let after = loaded.project;
    assert_eq!(after.root.parent_of(unfiled), None);
    assert_eq!(after.root.parent_of(root_item), Some(p.root.id));
    assert_eq!(after.sequence(second).unwrap().merged.as_ref().unwrap().video, Some(first));
    assert!(after.root.find_bin(empty).unwrap().children.is_empty());
    assert!(after.root.find_bin(BinId(to.as_u64().unwrap())).unwrap().find_bin(leaf).is_some());
    let view = loaded.view.unwrap();
    assert_eq!(view.open_sequences, vec![first, second]);
    assert_eq!(view.active_sequence, Some(second));
}
