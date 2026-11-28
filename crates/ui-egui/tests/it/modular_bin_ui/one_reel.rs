//! Exercise the normal trim command against a full-project reference and inspect
//! the actual session inventory independently of checkpoint I/O.
use super::*;
use filmcraft_engine::autosave::{AutosaveConfig, load_candidate};
use filmcraft_format::project_folder::{
    FolderDirectory, FolderSnapshot,
    index::{Index, cache_directory},
};
use filmcraft_project::{Bin, BinEntry};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

fn fixture(root: &Path, playhead: bool) -> PathBuf {
    let origin = root.join("origin");
    fs::create_dir_all(&origin).unwrap();
    let folder = origin.join("Feature");
    if let Some(src) = std::env::var_os("FILMCRAFT_TEST_TRIM_FIXTURE") {
        fs::create_dir(&folder).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                fs::copy(entry.path(), folder.join(entry.file_name())).unwrap();
            }
        }
        return folder.join("project.fcmod");
    }
    let f = Folder::create(&origin);
    let mut s = open(&f.path);
    let (reel, next) = s
        .edit("Prepare linked reel", |p, _| {
            let archive = p.add_bin("Archive", None);
            let next = p.new_sequence("Next tab", Default::default(), 1, 0, Some(archive));
            let hidden = p.new_sequence("Unrelated reel", Default::default(), 1, 0, Some(archive));
            let reel_bin = p.add_bin("Current cuts", None);
            let tracks = if playhead { 2 } else { 1 };
            let reel = p.new_sequence("Reel 3", Default::default(), tracks, tracks, Some(reel_bin));
            let matte = p.sequence(f.sequence).unwrap().video_tracks[0].items[0].item;
            let rate = p.sequence(reel).unwrap().settings.frame_rate;
            let range = TimeRange::new(Tick::ZERO, rate.tick_of(36));
            let link = p.alloc_id();
            let mut v = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
            let mut a = p.make_track_item(matte, TrackKind::Audio, Tick::ZERO, range, rate).unwrap();
            v.link = Some(link);
            a.link = Some(link);
            let later = p.make_track_item(matte, TrackKind::Video, range.end(), range, rate).unwrap();
            let q = p.sequence_mut(reel).unwrap();
            q.video_tracks[0].items.extend([v, later]);
            q.audio_tracks[0].items.push(a);
            if playhead {
                // W extracts from the middle of the sync-locked bed. The locked picture stays fixed.
                let long = TimeRange::new(Tick::ZERO, rate.tick_of(72));
                let bed = p.make_track_item(matte, TrackKind::Audio, Tick::ZERO, long, rate).unwrap();
                let locked = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, long, rate).unwrap();
                let q = p.sequence_mut(reel).unwrap();
                q.audio_tracks[1].sync_lock = true;
                q.audio_tracks[1].items.push(bed);
                q.video_tracks[1].locked = true;
                q.video_tracks[1].items.push(locked);
                let last = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
                p.sequence_mut(hidden).unwrap().video_tracks[0].items.push(last);
            }
            Ok((reel, next))
        })
        .unwrap();
    for id in [next, reel] {
        s.execute("sequence.open", json!({"item":id})).unwrap();
    }
    s.execute("file.save", json!({})).unwrap();
    f.path
}

fn owner(bin: &Bin, id: ItemId) -> Option<BinId> {
    bin.children.iter().find_map(|e| match e {
        BinEntry::Item(i) => (*i == id).then_some(bin.id),
        BinEntry::Bin(b) => owner(b, id),
    })
}

struct Run {
    folder: PathBuf,
    warm: Session,
    reference: Session,
    reel: ItemId,
    bin: BinId,
    deferred: Vec<ItemId>,
    args: Value,
}

impl Run {
    fn new(root: &Path) -> Self {
        Self::from_folder(root, fixture(root, false))
    }

    fn from_folder(root: &Path, folder: PathBuf) -> Self {
        let data = root.join("data");
        let directory = FolderDirectory::open(&folder).unwrap();
        let (index, ..) = Index::build(&directory, None).unwrap();
        fs::write(cache_directory(&folder, &data).unwrap().join("index.json"), index.bytes().unwrap()).unwrap();
        let reference = open(&folder);
        let reel = reference.state.active_sequence.unwrap();
        let bin = owner(&reference.project.root, reel).unwrap();
        let clip = &reference.project.sequence(reel).unwrap().video_tracks[0].items[0];
        let args = json!({"clip":clip.id, "edge":"out", "deltaFrames":-12});
        let mut warm = Session::local_folder_editor();
        warm.enable_experimental_modular_relink(data);
        warm.execute("file.open", json!({"path":folder})).unwrap();
        let deferred: Vec<_> = reference.project.sequences().map(|s| s.id).filter(|id| warm.project.item(*id).is_none()).collect();
        assert!(!deferred.is_empty());
        if std::env::var_os("FILMCRAFT_TEST_TRIM_FIXTURE").is_some() {
            eprintln!(
                "Active reel {reel:?}; resident sequences {:?}; deferred sequences {deferred:?}",
                warm.project.sequences().map(|s| s.id).collect::<Vec<_>>()
            );
        }
        Self { folder, warm, reference, reel, bin, deferred, args }
    }

    fn unloaded(&self) {
        assert!(self.deferred.iter().all(|id| self.warm.project.item(*id).is_none()));
    }

    fn trim_to_playhead(&mut self, command: &str) {
        let before = self.reference.project.clone();
        for s in [&mut self.reference, &mut self.warm] {
            playhead_trim(s, command, 12);
        }
        let cut = self.warm.project.sequence(self.reel).unwrap();
        assert_eq!(Some(cut), self.reference.project.sequence(self.reel));
        assert_eq!(self.warm.project.next_id, self.reference.project.next_id);
        assert!(self.warm.project.next_id > before.next_id);
        let prior = before.sequence(self.reel).unwrap();
        assert!(cut.video_tracks[0].items[1].start < prior.video_tracks[0].items[1].start);
        assert_eq!(cut.video_tracks[0].items[0].link, cut.audio_tracks[0].items[0].link);
        if prior.video_tracks[1].locked {
            assert_eq!(cut.video_tracks[1], prior.video_tracks[1]);
            if command == "trim.rippleNext" {
                assert!(cut.audio_tracks[1].items.len() > prior.audio_tracks[1].items.len());
            }
        }
        assert_eq!(self.warm.dirty_folder_bins(), [self.bin].into());
        self.unloaded();
    }

    fn use_ripple_extension(&mut self) {
        self.args["mode"] = json!("ripple");
        self.args["deltaFrames"] = json!(12);
    }

    fn trim(&mut self) {
        for s in [&mut self.reference, &mut self.warm] {
            s.execute("playhead.set", json!({"frame":2})).unwrap();
            s.execute("timeline.select", json!({"clips":[self.args["clip"]]})).unwrap();
            let delta = s.execute("timeline.trim", self.args.clone()).unwrap();
            assert_eq!(delta["delta"], json!(s.sequence_rate().tick_of(self.args["deltaFrames"].as_i64().unwrap())));
        }
        assert_eq!(self.warm.project.sequence(self.reel), self.reference.project.sequence(self.reel));
        assert_eq!(self.warm.dirty_folder_bins(), [self.bin].into());
        self.unloaded();
    }
}

fn manifest(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn save_and_discard_keep_other_reels_unloaded() {
    check_save_and_discard("regular");
}

#[test]
fn ripple_save_and_discard_keep_other_reels_unloaded() {
    check_save_and_discard("ripple");
}

fn check_save_and_discard(mode: &str) {
    for save in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Run::new(tmp.path());
        let prior = manifest(&r.folder);
        let before = r.reference.project.clone();
        if mode == "ripple" {
            r.use_ripple_extension();
        }
        r.trim();
        let start = |project: &filmcraft_project::Project| project.sequence(r.reel).unwrap().video_tracks[0].items[1].start;
        let shift = if mode == "ripple" { r.warm.sequence_rate().tick_of(12) } else { Tick::ZERO };
        assert_eq!(start(&r.warm.project), start(&before) + shift);
        r.warm.execute("edit.undo", json!({})).unwrap();
        assert_eq!(r.warm.project.sequence(r.reel), before.sequence(r.reel));
        assert!(!r.warm.is_dirty());
        r.unloaded();
        r.warm.execute("edit.redo", json!({})).unwrap();
        r.unloaded();
        if save {
            r.warm.execute("file.save", json!({})).unwrap();
            let next = manifest(&r.folder);
            let changed: Vec<_> =
                prior["items"].as_array().unwrap().iter().zip(next["items"].as_array().unwrap()).filter(|(a, b)| a != b).map(|(a, _)| a[0].clone()).collect();
            assert_eq!(changed, [json!(r.reel)]);
            assert_eq!(prior["bins"], next["bins"]);
            assert_eq!(r.warm.execute("file.save", json!({})).unwrap()["bytes"], 0);
            assert_eq!(open(&r.folder).project_view(), r.reference.project_view());
        } else {
            r.warm.execute("project.discardBin", json!({"bin":r.bin})).unwrap();
            assert_eq!(manifest(&r.folder), prior);
        }
        r.unloaded();
        assert!(!r.warm.is_dirty());
        assert_eq!(open(&r.folder).project, if save { r.reference.project } else { before });
    }
}

fn playhead_trim(s: &mut Session, command: &str, frame: i64) {
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("timeline.select", json!({"clips":[clip]})).unwrap();
    s.execute("playhead.set", json!({"frame":frame})).unwrap();
    s.execute(command, json!({})).unwrap();
}

fn another_split(s: &mut Session, next: u64) {
    assert_eq!(s.project.next_id, next);
    playhead_trim(s, "trim.ripplePrevious", 1);
    let id = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    assert!(id >= next && id < s.project.next_id);
}

#[test]
fn playhead_trims_save_discard_and_reopen() {
    for command in ["trim.ripplePrevious", "trim.rippleNext"] {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Run::from_folder(tmp.path(), fixture(tmp.path(), true));
        let prior = manifest(&r.folder);
        let before = r.reference.project.clone();
        r.trim_to_playhead(command);
        r.warm.execute("edit.undo", json!({})).unwrap();
        assert_eq!(r.warm.project.next_id, before.next_id);
        assert_eq!(r.warm.project.sequence(r.reel), before.sequence(r.reel));
        r.unloaded();
        r.warm.execute("edit.redo", json!({})).unwrap();
        assert_eq!(r.warm.project.sequence(r.reel), r.reference.project.sequence(r.reel));
        assert_eq!(r.warm.project.next_id, r.reference.project.next_id);
        r.unloaded();
        r.warm.execute("project.discardBin", json!({"bin":r.bin})).unwrap();
        assert_eq!(r.warm.project.next_id, before.next_id);
        assert_eq!(r.warm.project.sequence(r.reel), before.sequence(r.reel));
        assert!(!r.warm.is_dirty());
        assert_eq!(manifest(&r.folder), prior);
        r.unloaded();
        playhead_trim(&mut r.warm, command, 12);
        if command == "trim.ripplePrevious" {
            r.warm.execute("project.saveBin", json!({"bin":r.bin})).unwrap();
        } else {
            r.warm.execute("file.save", json!({})).unwrap();
        }
        let next = manifest(&r.folder);
        assert_eq!(next["bins"], prior["bins"]);
        let changed: Vec<_> =
            next["items"].as_array().unwrap().iter().zip(prior["items"].as_array().unwrap()).filter(|(a, b)| a != b).map(|(a, _)| &a[0]).collect();
        assert_eq!(changed, [&json!(r.reel)]);
        let envelope = manifest(&r.folder.with_file_name(next["envelope"].as_str().unwrap()));
        assert_eq!(envelope["project"]["next_id"], r.reference.project.next_id);
        assert_eq!(open(&r.folder).project, r.reference.project);
        assert!(!r.warm.is_dirty());
        r.unloaded();
        r.warm = Session::local_folder_editor();
        r.warm.enable_experimental_modular_relink(tmp.path().join("data"));
        r.warm.execute("file.open", json!({"path":r.folder})).unwrap();
        another_split(&mut r.warm, r.reference.project.next_id);
        playhead_trim(&mut r.reference, "trim.ripplePrevious", 1);
        assert_eq!(r.warm.project.sequence(r.reel), r.reference.project.sequence(r.reel));
        r.unloaded();
    }
}

#[test]
fn timeline_q_and_w_trim_only_the_active_reel() {
    for (key, command) in [("Q", "trim.ripplePrevious"), ("W", "trim.rippleNext")] {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Run::from_folder(tmp.path(), fixture(tmp.path(), true));
        playhead_trim(&mut r.reference, command, 12);
        let mut d = Driver::open(r.warm, support::with_shots(support::builder()));
        d.exec("timeline.select", json!({"clips":[r.args["clip"]]}));
        d.exec("playhead.set", json!({"frame":12}));
        d.ok("ui.set", json!({"focused":"Timeline"}));
        d.ok("ui.key", json!({"key":key}));
        d.frames(4);
        let s = &d.app().session;
        assert_eq!(s.project.sequence(r.reel), r.reference.project.sequence(r.reel));
        assert_eq!(s.dirty_folder_bins(), [r.bin].into());
        assert!(r.deferred.iter().all(|id| s.project.item(*id).is_none()));
    }
}

#[test]
fn split_uses_ids_above_a_deferred_reel_when_the_stored_counter_lags() {
    use sha2::{Digest, Sha256};
    let tmp = tempfile::tempdir().unwrap();
    let folder = fixture(tmp.path(), true);
    let mut head = manifest(&folder);
    let mut envelope = manifest(&folder.with_file_name(head["envelope"].as_str().unwrap()));
    envelope["project"]["next_id"] = json!(1);
    let bytes = serde_json::to_vec(&envelope).unwrap();
    let key = format!("{:x}", Sha256::digest(&bytes));
    fs::write(folder.with_file_name(&key), bytes).unwrap();
    head["envelope"] = json!(key);
    fs::write(&folder, serde_json::to_vec(&head).unwrap()).unwrap();
    let mut r = Run::from_folder(tmp.path(), folder);
    let max = r.reference.project.max_id();
    assert!(r.deferred.iter().any(|id| r.reference.project.item(*id).unwrap().max_id() == max));
    r.trim_to_playhead("trim.ripplePrevious");
    assert!(r.warm.project.sequence(r.reel).unwrap().video_tracks[0].items[0].id.0 > max);
}

#[test]
fn promoting_after_split_undo_preserves_ids_for_redo() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = Run::from_folder(tmp.path(), fixture(tmp.path(), true));
    r.trim_to_playhead("trim.rippleNext");
    r.warm.execute("edit.undo", json!({})).unwrap();
    r.unloaded();
    r.warm.execute("file.projectSettings.general", json!({})).unwrap();
    r.warm.execute("edit.redo", json!({})).unwrap();
    assert_eq!(r.warm.project, r.reference.project);
    another_split(&mut r.warm, r.reference.project.next_id);
}

const ACTION: &str = "FILMCRAFT_TEST_TRIM_ACTION";
const ROOT: &str = "FILMCRAFT_TEST_TRIM_ROOT";

fn child(root: &Path, action: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "modular_bin_ui::one_reel::crash_recovery", "--nocapture"]).env(ACTION, action).env(ROOT, root);
    c
}

fn checkpoint(root: &Path, action: &str) {
    let playhead = matches!(action, "trim.ripplePrevious" | "trim.rippleNext");
    let mut r = Run::from_folder(root, fixture(root, playhead));
    if action == "ripple" {
        r.use_ripple_extension();
    }
    let disk = fs::read(&r.folder).unwrap();
    r.warm.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
    if playhead {
        r.trim_to_playhead(action);
        fs::write(root.join("edited-reel"), serde_json::to_vec(&r.reel).unwrap()).unwrap();
    } else {
        r.trim();
    }
    assert_eq!(r.warm.persistence.as_ref().unwrap().flush().unwrap(), Some(r.warm.revision));
    r.unloaded();
    assert!(r.warm.is_dirty());
    let (_, mut expected) = FolderSnapshot::open(&r.folder).unwrap();
    expected.project.next_id = r.reference.project.next_id;
    expected.project.items.insert(r.reel, r.reference.project.item(r.reel).unwrap().clone());
    fs::write(root.join("expected.fcproj"), filmcraft_format::encode(&expected.project, false)).unwrap();
    if action == "discard" {
        r.warm.execute("project.discardBin", json!({"bin":r.bin})).unwrap();
        assert_eq!(r.warm.persistence.as_ref().unwrap().flush().unwrap(), None);
    } else if action == "interrupt" {
        let id = r.deferred[0];
        let head = manifest(&r.folder);
        let key = head["items"].as_array().unwrap().iter().find(|pair| pair[0] == json!(id)).unwrap()[1].as_str().unwrap();
        fs::rename(r.folder.with_file_name(key), root.join("unread-record")).unwrap();
        r.warm.execute("timeline.trim", json!({"deltaFrames":-1,"clip":r.args["clip"], "edge":"out"})).unwrap();
        assert!(r.warm.persistence.as_ref().unwrap().flush().is_err());
        assert!(r.warm.is_dirty());
    }
    assert_eq!(fs::read(&r.folder).unwrap(), disk);
    r.unloaded();
    println!("confirmed trim boundary");
    std::thread::park();
}

#[test]
fn crash_recovery() {
    if let Ok(action) = std::env::var(ACTION) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        if !action.starts_with("recover") {
            checkpoint(&root, &action);
            return;
        }
        assert!(!root.join("origin/Feature/project.fcmod").exists());
        let mut s = Session::local_folder_editor();
        s.start_autosave(AutosaveConfig::new(root.join("data"))).unwrap();
        if action == "recover-discard" {
            assert!(s.recovery_candidates().is_empty());
            return;
        }
        let expected = filmcraft_format::decode(&fs::read(root.join("expected.fcproj")).unwrap()).unwrap().project;
        assert_eq!(s.recovery_candidates().len(), 1);
        let candidate = &s.recovery_candidates()[0];
        assert!(candidate.meta.reviewed_source.is_none());
        assert_eq!(load_candidate(candidate).unwrap().project, expected);
        let detached = filmcraft_format::project_folder::detached_project(expected, &root.join("origin/Feature/project.fcmod"), None).unwrap();
        s.execute("file.recover", json!({})).unwrap();
        assert!(s.is_dirty());
        for cut in detached.sequences() {
            assert_eq!(s.project.item(cut.id), Some(cut));
        }
        assert_eq!(s.project.settings, detached.settings);
        let saved = root.join("Recovered.fcproj");
        s.execute("file.saveAs", json!({"path":saved})).unwrap();
        let mut reopened = open(&saved);
        for cut in reopened.project.sequences() {
            assert_eq!(detached.item(cut.id), Some(cut));
        }
        if root.join("edited-reel").exists() {
            let reel: ItemId = serde_json::from_slice(&fs::read(root.join("edited-reel")).unwrap()).unwrap();
            reopened.execute("sequence.open", json!({"item":reel})).unwrap();
            another_split(&mut reopened, detached.next_id);
        }
        return;
    }
    for action in ["trim.ripplePrevious", "trim.rippleNext", "checkpoint", "ripple", "interrupt", "discard"] {
        let tmp = tempfile::tempdir().unwrap();
        let mut writer = child(tmp.path(), action).stdout(Stdio::piped()).spawn().unwrap();
        let ready = BufReader::new(writer.stdout.take().unwrap()).lines().map_while(Result::ok).any(|l| l.contains("confirmed trim boundary"));
        if ready {
            writer.kill().unwrap();
        }
        writer.wait().unwrap();
        assert!(ready, "{action} exited before acknowledgement");
        fs::rename(tmp.path().join("origin"), tmp.path().join("disconnected")).unwrap();
        assert!(child(tmp.path(), if action == "discard" { "recover-discard" } else { "recover" }).status().unwrap().success());
    }
}

#[test]
fn closing_the_edited_bin_loads_only_the_next_tab() {
    for save in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Run::new(tmp.path());
        let original = r.reference.project.clone();
        r.trim();
        let mut d = Driver::open(r.warm, support::with_shots(support::builder()));
        invoke(&mut d, "openBin", json!({"bin":r.bin,"how":"window"}));
        let unneeded =
            *r.deferred.iter().find(|id| owner(&r.reference.project.root, **id) != Some(r.bin) && !d.app().session.state.open_sequences.contains(id)).unwrap();
        assert!(d.app().session.project.item(unneeded).is_none(), "opening the edited bin loaded an unrelated reel");
        invoke(&mut d, "closeBinTab", json!({"bin":r.bin}));
        if save {
            let head = manifest(&r.folder);
            let key = head["items"].as_array().unwrap().iter().find(|r| r[0] == json!(unneeded)).unwrap()[1].as_str().unwrap();
            let record = r.folder.with_file_name(key);
            let missing = tmp.path().join("unavailable");
            fs::rename(&record, &missing).unwrap();
            d.click("project.closeBin.save");
            assert!(d.app().session.is_dirty());
            assert!(d.app().session.state.open_sequences.contains(&r.reel));
            assert_eq!(manifest(&r.folder), head);
            fs::rename(missing, record).unwrap();
            invoke(&mut d, "closeBinTab", json!({"bin":r.bin}));
        }
        d.click(if save { "project.closeBin.save" } else { "project.closeBin.discard" });
        assert!(!d.app().session.state.open_sequences.contains(&r.reel));
        assert!(!d.app().session.is_dirty());
        assert!(d.app().session.project.item(unneeded).is_none());
        assert_eq!(open(&r.folder).project, if save { r.reference.project } else { original });
    }
}

#[test]
fn dragging_a_linked_clip_edge_keeps_unrelated_reels_unloaded() {
    check_edge_drag(false);
}

#[test]
fn dragging_a_ripple_edge_keeps_unrelated_reels_unloaded() {
    check_edge_drag(true);
}

fn check_edge_drag(ripple: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let r = Run::new(tmp.path());
    let clip = r.args["clip"].as_u64().unwrap();
    let prior = r.reference.project.sequence(r.reel).unwrap().video_tracks[0].items[0].duration;
    let mut d = Driver::open(r.warm, support::with_shots(support::builder()));
    if ripple {
        d.exec("tool.ripple", json!({}));
    }
    let later = r.reference.project.sequence(r.reel).unwrap().video_tracks[0].items[1].start;
    let edges = ["in", "out"].map(|edge| d.ok("ui.timeline.locate", json!({"clip":clip,"edge":edge}))["x"].as_f64().unwrap() as f32);
    let [_, top, _, _] = d.expect_rect(&format!("timeline.clip.{clip}"));
    let y = top as f32 + 8.0;
    let out = edges[1] + 2.0;
    let from = egui::pos2(out - 3.0, y);
    let to = egui::pos2(from.x - (out - edges[0] + 2.0) / 3.0, y);
    support::pointer_drag(&mut d, from, to, 8);
    let q = d.app().session.project.sequence(r.reel).unwrap();
    let duration = q.video_tracks[0].items[0].duration;
    assert!(duration < prior);
    assert_eq!(q.video_tracks[0].items[1].start, if ripple { later + (duration - prior) } else { later });
    assert_eq!(q.audio_tracks[0].items[0].duration, duration);
    assert!(r.deferred.iter().all(|id| d.app().session.project.item(*id).is_none()));
    d.exec("file.save", json!({}));
    assert_eq!(open(&r.folder).project.sequence(r.reel).unwrap().video_tracks[0].items[0].duration, duration);
    assert!(r.deferred.iter().all(|id| d.app().session.project.item(*id).is_none()));
}

#[test]
fn a_full_project_command_preserves_the_trim_on_redo() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = Run::new(tmp.path());
    r.trim();
    r.warm.execute("edit.undo", json!({})).unwrap();
    r.unloaded();
    r.warm.execute("file.projectSettings.general", json!({})).unwrap();
    r.warm.execute("edit.redo", json!({})).unwrap();
    assert_eq!(r.warm.project, r.reference.project);
    assert_eq!(r.warm.dirty_folder_bins(), [r.bin].into());
}

#[test]
fn nudging_a_selected_ripple_edge_keeps_unrelated_reels_unloaded() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = Run::new(tmp.path());
    r.args["mode"] = json!("ripple");
    r.reference.execute("timeline.trim", r.args.clone()).unwrap();
    r.warm.execute("trim.selectEditPoint", json!({"clip":r.args["clip"],"edge":"out","kind":"ripple"})).unwrap();
    r.warm.execute("trim.nudge", json!({"frames":-12})).unwrap();
    assert_eq!(r.warm.project.sequence(r.reel), r.reference.project.sequence(r.reel));
    assert_eq!(r.warm.dirty_folder_bins(), [r.bin].into());
    r.unloaded();
}
