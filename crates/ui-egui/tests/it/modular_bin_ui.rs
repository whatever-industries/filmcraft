//! An explicit bin close in the modular editor commits one decision before closing timelines.
use crate::support::{self, Drive, Launch};
use egui_kittest::kittest::{NodeT, Queryable};
use filmcraft_engine::Session;
use filmcraft_project::{BinId, ItemId, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

support::driver!(Driver);

struct Folder {
    path: PathBuf,
    cuts: BinId,
    selects: BinId,
    master: ItemId,
    sequence: ItemId,
}

fn open(path: &Path) -> Session {
    let mut s = Session::local_folder_editor();
    s.execute("file.open", json!({"path":path})).unwrap();
    s
}

impl Folder {
    fn create(dir: &Path) -> Self {
        let mut session = Session::default();
        let matte = session.execute("file.newColorMatte", json!({"color":"#3377bb","width":160,"height":90,"seconds":2.0})).unwrap();
        let matte = ItemId(matte["item"].as_u64().unwrap());
        let mut p = (*session.project).clone();
        let cuts = p.add_bin("Cuts", None);
        let selects = p.add_bin("Selects", None);
        let settings = filmcraft_project::SequenceSettings { width: 160, height: 90, ..Default::default() };
        let sequence = p.new_sequence("Interview", settings.clone(), 1, 0, Some(selects));
        let master = p.new_sequence("Master", settings, 1, 0, Some(cuts));
        let rate = p.sequence(master).unwrap().settings.frame_rate;
        let source = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(5)), rate).unwrap();
        p.sequence_mut(sequence).unwrap().video_tracks[0].items.push(source);
        let clip = p.make_track_item(sequence, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(5)), rate).unwrap();
        p.sequence_mut(master).unwrap().video_tracks[0].items.push(clip);
        let path = dir.join("Feature");
        let standard = dir.join("Feature.fcproj");
        fs::write(&standard, filmcraft_format::encode(&p, false)).unwrap();
        filmcraft_format::project_folder::pack(&standard, &path, filmcraft_format::atomic_write_new).unwrap();
        Self { path: path.join("project.fcmod"), cuts, selects, master, sequence }
    }

    fn driver(&self) -> Driver {
        Driver::open(open(&self.path), support::with_shots(support::builder()))
    }

    fn edit(&self, d: &mut Driver) {
        for (bin, id) in [(self.cuts, self.master), (self.selects, self.sequence)] {
            invoke(d, "openBin", json!({"bin":bin,"how":"inPlace"}));
            d.exec("sequence.open", json!({"item":id}));
            d.app()
                .session
                .edit("Change start timecode", |p, _| {
                    p.sequence_mut(id).unwrap().start_timecode = 24;
                    Ok(())
                })
                .unwrap();
        }
        d.exec("file.projectSettings.general", json!({"titleSafe":[12,14]}));
        d.frames(3);
    }
}

fn invoke(d: &mut Driver, op: &str, params: Value) -> Value {
    let answer = d.ok("ui.menu.invoke", json!({"id":format!("projectPanel.{op}"),"params":params}));
    d.frames(3);
    answer
}

#[test]
fn save_from_the_keyboard_closes_only_the_selected_bin_and_its_timelines() {
    let dir = tempfile::tempdir().unwrap();
    let f = Folder::create(dir.path());
    let mut d = f.driver();
    f.edit(&mut d);
    let mut original = open(&f.path);
    original.execute("sequence.open", json!({"item":f.master})).unwrap();
    original.set_playhead(Tick::ZERO);
    let reference = original.render_program(1.0).unwrap().over_black_rgba8();
    let tab = d.harness.get_by_role_and_label(egui::accesskit::Role::Tab, "Bin: Selects (Unsaved)");
    assert_eq!(tab.accesskit_node().is_selected(), Some(true));
    d.exec("shortcuts.set", json!({"command":"projectPanel.closeBinTab","keys":"Shift+F8","panel":"Project"}));
    d.ok("ui.set", json!({"focused":"Project"}));
    d.ok("ui.key", json!({"key":"Shift+F8"}));
    d.frames(4);
    assert!(d.harness.get_by_role_and_label(egui::accesskit::Role::Button, "Save").is_focused());
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::Button, "Discard");
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::Dialog, "Save changes to Selects?");
    support::save_shot(&mut d, 0, "modular-close-selects.png");
    d.ok("ui.key", json!({"key":"Enter"}));
    d.frames(4);
    let s = &mut d.app().session;
    assert!(s.state.open_sequences.contains(&f.master));
    assert!(!s.state.open_sequences.contains(&f.sequence));
    assert_eq!(s.project.sequence(f.master).unwrap().video_tracks[0].items[0].item, f.sequence);
    assert_eq!(s.state.active_sequence, Some(f.master));
    s.set_playhead(Tick::ZERO);
    let picture = s.render_program(1.0).unwrap().over_black_rgba8();
    assert!(reference.as_chunks::<4>().0.iter().any(|pixel| pixel[2] > pixel[0]));
    assert_eq!(picture, reference);
    let reopened = open(&f.path);
    assert_eq!(reopened.project.sequence(f.sequence).unwrap().start_timecode, 24);
    assert_eq!(reopened.project.sequence(f.master), original.project.sequence(f.master));
    assert_eq!(reopened.project.settings, original.project.settings);
    assert_eq!(d.exec("project.binStatus", json!({}))["dirtyBins"], json!([f.cuts]));
    assert_eq!(d.app().ui.project_panel.tabs.len(), 1);
}

#[test]
fn navigating_relocating_and_restoring_never_duplicates_a_bin_or_redirects_a_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let f = Folder::create(dir.path());
    let mut d = f.driver();
    f.edit(&mut d);
    let before = fs::read(&f.path).unwrap();
    invoke(&mut d, "openBin", json!({"bin":f.cuts,"how":"window"}));
    invoke(&mut d, "openBin", json!({"bin":f.cuts}));
    let pp = &d.app().ui.project_panel;
    assert_eq!(pp.tabs.len(), 2);
    assert!(pp.tabs.iter().any(|t| t.bin == f.cuts.0 && t.floating));
    invoke(&mut d, "openBin", json!({"bin":f.selects,"how":"tab"}));
    d.frames(3);
    d.click("project.tab.1.close");
    // Restore a layout in a different order, with a legacy duplicate.
    d.ok("ui.set", json!({"projectPanel":{"tabs":[{"bin":f.selects},{"bin":f.cuts,"floating":true},{"bin":f.selects,"floating":true}],"activeTab":0}}));
    assert_eq!(d.app().ui.project_panel.tabs.len(), 2);
    d.click("project.closeBin.discard");
    assert_eq!(fs::read(&f.path).unwrap(), before);
    let pp = &d.app().ui.project_panel;
    assert_eq!(pp.tabs.len(), 1);
    assert_eq!(pp.tabs[0].bin, f.cuts.0);
    assert!(d.app().session.state.open_sequences.contains(&f.master));
    assert!(!d.app().session.state.open_sequences.contains(&f.sequence));
    d.exec("edit.undo", json!({}));
    d.exec("edit.redo", json!({}));
    assert_eq!(d.app().session.project.sequence(f.sequence), open(&f.path).project.sequence(f.sequence));
}

#[test]
fn failed_save_keeps_bin_open_and_a_published_save_retries_cleanup_only() {
    let dir = tempfile::tempdir().unwrap();
    let f = Folder::create(dir.path());
    let mut d = f.driver();
    let mut cfg = filmcraft_engine::autosave::AutosaveConfig::new(dir.path());
    cfg.journal_debounce = std::time::Duration::from_secs(60);
    d.app().session.start_autosave(cfg).unwrap();
    invoke(&mut d, "openBin", json!({"bin":f.selects,"how":"window"}));
    d.exec("sequence.open", json!({"item":f.sequence}));
    d.exec("project.renameBin", json!({"bin":f.selects,"name":"Selects renamed"}));
    let published = fs::read(&f.path).unwrap();
    let archive = f.path.with_file_name(d.app().session.folder_revision().unwrap());
    fs::create_dir(&archive).unwrap();
    d.frames(3);
    d.harness.get_by_role_and_label(egui::accesskit::Role::Button, "Close window").click();
    d.frames(4);
    d.click("project.closeBin.save");
    assert_eq!(fs::read(&f.path).unwrap(), published);
    assert!(d.app().session.is_dirty());
    assert_eq!(d.app().ui.project_panel.tabs.len(), 1);
    fs::remove_dir(archive).unwrap();
    let snapshot = d.app().session.persistence.as_ref().unwrap().session_dir.join("session.json");
    d.app().session.persistence.as_ref().unwrap().flush().unwrap();
    fs::rename(&snapshot, snapshot.with_extension("previous")).unwrap();
    fs::create_dir(&snapshot).unwrap();
    invoke(&mut d, "closeBinTab", json!({"bin":f.selects}));
    d.click("project.closeBin.save");
    let saved = fs::read(&f.path).unwrap();
    assert_ne!(saved, published);
    assert!(d.app().session.bin_needs_cleanup(f.selects));
    assert!(d.app().session.state.open_sequences.contains(&f.sequence));
    assert_eq!(d.app().ui.project_panel.tabs.len(), 1);
    fs::remove_dir(snapshot).unwrap();
    invoke(&mut d, "closeBinTab", json!({"bin":f.selects}));
    assert_eq!(fs::read(&f.path).unwrap(), saved);
    assert!(d.app().ui.project_panel.tabs.is_empty());
    assert!(!d.app().session.state.open_sequences.contains(&f.sequence));
}

#[test]
fn stale_revision_and_project_switch_do_not_close_an_unrelated_view() {
    let dir = tempfile::tempdir().unwrap();
    let f = Folder::create(dir.path());
    let mut d = f.driver();
    f.edit(&mut d);
    let mut other = open(&f.path);
    other.execute("project.renameBin", json!({"bin":f.cuts,"name":"Other writer"})).unwrap();
    other.execute("file.save", json!({})).unwrap();
    let published = fs::read(&f.path).unwrap();
    invoke(&mut d, "closeBinTab", json!({"bin":f.selects}));
    d.click("project.closeBin.discard");
    assert_eq!(d.app().session.project.sequence(f.sequence).unwrap().start_timecode, 24);
    assert!(d.app().session.state.open_sequences.contains(&f.sequence));
    assert_eq!(d.app().ui.project_panel.tabs.len(), 2);
    invoke(&mut d, "closeBinTab", json!({"bin":f.selects}));
    d.exec("file.open", json!({"path":f.path}));
    d.frames(3);
    assert!(d.element("project.closeBin.save").is_none());
    assert_eq!(fs::read(&f.path).unwrap(), published);
}

#[test]
fn list_and_icon_drop_require_an_open_destination() {
    for mode in ["list", "icon"] {
        let dir = tempfile::tempdir().unwrap();
        let f = Folder::create(dir.path());
        let mut d = f.driver();
        d.ok("ui.panel.show", json!({"panel":"Project"}));
        d.ok("ui.menu.invoke", json!({"id":"window.maximizeFrame","params":{}}));
        d.exec("project.view.set", json!({"view":mode}));
        // Drag the entire Cuts bin so the same source is visible in list and icon view.
        let initial = fs::read(&f.path).unwrap();
        let history = d.app().session.history.undo.len();
        d.app().ui.expanded_bins = vec![f.selects.0];
        for open_dest in [false, true] {
            if open_dest {
                invoke(&mut d, "openBin", json!({"bin":f.selects,"how":"tab"}));
                d.click("project.tab.project");
            }
            d.frames(3);
            let point = |d: &mut Driver, bin: BinId| {
                let [x, y, w, h] = d.element(&format!("project.bin.{}", bin.0)).unwrap();
                json!({"x":x+80.0_f32.min(w/2.0),"y":y+h/2.0})
            };
            let from = point(&mut d, f.cuts);
            let to = point(&mut d, f.selects);
            d.ok("ui.drag", json!({"from":from,"to":to,"steps":14}));
            d.frames(3);
            let s = &d.app().session;
            assert_eq!(s.project.root.find_bin(f.selects).unwrap().find_bin(f.cuts).is_some(), open_dest, "{mode}");
            assert_eq!(s.history.undo.len(), history + usize::from(open_dest));
            assert_eq!(s.is_dirty(), open_dest);
            assert_eq!(fs::read(&f.path).unwrap(), initial);
        }
    }
}

#[test]
fn killing_the_editor_after_close_recovers_cuts_and_its_setting() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    const CHILD: &str = "FILMCRAFT_BIN_CLOSE_TEST_DIR";
    if let Some(dir) = std::env::var_os(CHILD) {
        let root = PathBuf::from(dir);
        let f = Folder::create(&root);
        let mut d = f.driver();
        d.app().session.start_autosave(filmcraft_engine::autosave::AutosaveConfig::new(&root)).unwrap();
        f.edit(&mut d);
        d.app().session.persistence.as_ref().unwrap().flush().unwrap();
        invoke(&mut d, "closeBinTab", json!({"bin":f.selects}));
        d.click("project.closeBin.discard");
        assert!(!d.app().session.state.open_sequences.contains(&f.sequence));
        println!("bin close acknowledged");
        std::thread::park();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "modular_bin_ui::killing_the_editor_after_close_recovers_cuts_and_its_setting", "--nocapture"])
        .env(CHILD, dir.path())
        .env_remove("FILMCRAFT_UI_SHOTS")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let acknowledged = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|l| l.contains("bin close acknowledged"));
    if acknowledged {
        child.kill().unwrap();
    }
    child.wait().unwrap();
    assert!(acknowledged);
    let live = open(&dir.path().join("Feature/project.fcmod")).project;
    let mut s = Session::local_folder_editor();
    s.start_autosave(filmcraft_engine::autosave::AutosaveConfig::new(dir.path())).unwrap();
    s.execute("file.recover", json!({})).unwrap();
    let master = live.sequences().find(|i| i.name == "Master").unwrap().id;
    let nested = live.sequence(master).unwrap().video_tracks[0].items[0].item;
    assert_eq!(s.project.sequence(master).unwrap().start_timecode, 24);
    assert_eq!(s.project.sequence(nested), live.sequence(nested));
    assert_eq!(s.project.settings.title_safe, (12.0, 14.0));
    assert_ne!(s.project.settings.title_safe, live.settings.title_safe);
    s.shutdown();
}

#[test]
fn global_save_from_bin_focus_writes_every_edit_and_clean_parent_close_keeps_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let f = Folder::create(dir.path());
    let mut d = f.driver();
    d.exec("project.moveToBin", json!({"items":[f.selects],"bin":f.cuts}));
    d.exec("file.save", json!({}));
    f.edit(&mut d);
    d.ok("ui.key", json!({"key":"Cmd+S"}));
    d.frames(3);
    let published = open(&f.path);
    assert_eq!(published.project.sequence(f.master).unwrap().start_timecode, 24);
    assert_eq!(published.project.sequence(f.sequence).unwrap().start_timecode, 24);
    assert_eq!(published.project.settings.title_safe, (12.0, 14.0));
    assert!(published.project.root.find_bin(f.cuts).unwrap().find_bin(f.selects).is_some());
    invoke(&mut d, "closeBinTab", json!({"bin":f.cuts}));
    assert!(d.element("project.closeBin.save").is_none());
    assert_eq!(d.app().ui.project_panel.tabs.len(), 1);
    assert_eq!(d.app().ui.project_panel.tabs[0].bin, f.selects.0);
    assert_eq!(d.app().session.state.open_sequences, vec![f.sequence]);
}

#[test]
fn destination_only_save_survives_a_killed_editor() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    const CHILD: &str = "FILMCRAFT_BIN_MOVE_CHILD";
    if let Some(dir) = std::env::var_os(CHILD) {
        let f = Folder::create(Path::new(&dir));
        let mut d = f.driver();
        invoke(&mut d, "openBin", json!({"bin":f.cuts}));
        d.exec("project.moveToBin", json!({"items":[f.sequence],"bin":f.cuts}));
        d.app()
            .session
            .edit("Edit moved sequence", |p, _| {
                p.sequence_mut(f.sequence).unwrap().start_timecode = 42;
                Ok(())
            })
            .unwrap();
        invoke(&mut d, "closeBinTab", json!({"bin":f.cuts}));
        d.click("project.closeBin.save");
        assert!(d.app().ui.project_panel.tabs.iter().all(|t| t.bin != f.cuts.0));
        println!("destination published");
        std::thread::park();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "modular_bin_ui::destination_only_save_survives_a_killed_editor", "--nocapture"])
        .env(CHILD, dir.path())
        .env_remove("FILMCRAFT_UI_SHOTS")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let saved = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|l| l.contains("destination published"));
    if saved {
        child.kill().unwrap();
    }
    child.wait().unwrap();
    assert!(saved);
    let p = open(&dir.path().join("Feature/project.fcmod")).project;
    let master = p.sequences().find(|i| i.name == "Master").unwrap().as_sequence().unwrap();
    let original = master.video_tracks[0].items[0].item;
    assert_eq!(p.sequence(original).unwrap().start_timecode, 42);
    let older = p.sequences().find(|i| i.id != original && i.name != "Master").unwrap();
    assert_eq!(older.as_sequence().unwrap().start_timecode, 0);
    assert_ne!(p.root.parent_of(original), p.root.parent_of(older.id));
}

mod transfer_playback;

#[test]
fn modular_recovery_limits_can_be_disabled_in_project_settings() {
    let dir = tempfile::tempdir().unwrap();
    let f = Folder::create(dir.path());
    let mut d = Driver::open(open(&f.path), support::builder());
    d.ok("ui.menu.invoke", json!({"id":"file.projectSettings.general"}));
    d.click("projectSettings.tab.recovery");
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Bin backups per project");
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Versions per bin");
    d.click("projectSettings.recovery.preview");
    let _ = d.harness.get_by_label_contains("Versions eligible for cleanup");
    d.click("projectSettings.recovery.enabled");
    d.click("projectSettings.ok");
    assert_eq!(open(&f.path).execute("project.recoveryRetention", json!({})).unwrap()["enabled"], false);
}
