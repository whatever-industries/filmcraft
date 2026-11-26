//! Headless UI tests of File ▸ Open Project, File ▸ Open Recent and Home's Recent list with unsaved
//! changes: once the project to open is chosen, the Save Project prompt asks Cancel / Don't Save /
//! Save before it opens.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use crate::support::{Drive, Launch, driver};
use filmcraft_engine::project::{ClipId, Label};
use filmcraft_ui_egui::native_dialogs::FileDialog;
use filmcraft_ui_egui::state::Mode;
use serde_json::{Value, json};

driver! {
    Driver;
    const CLICK_SETTLE: usize = 3;
}

impl Driver {
    fn path(&mut self) -> Option<String> {
        self.app().session.path.clone()
    }

    /// Make an unsaved change: the first clip turns yellow. Returns the clip.
    fn edit(&mut self) -> ClipId {
        let c = self.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
        self.exec("timeline.select", json!({"clips": [c.0]}));
        self.menu("edit.label.yellow");
        assert!(self.app().session.is_dirty());
        c
    }

    /// The label of `clip` in the open project.
    fn label(&mut self, clip: ClipId) -> Label {
        let p = &self.app().session.project;
        p.sequences().filter_map(|i| p.sequence(i.id)).find_map(|q| q.find_item(clip).map(|(_, it)| it.label)).unwrap()
    }

    /// File ▸ Open Project… with the Open dialog answering `path`.
    fn open_project(&mut self, path: &str) -> Value {
        let path = path.to_string();
        self.app().hooks.pick_open_project = Some(Box::new(move || Some(path.clone())));
        self.menu("file.open")
    }

    /// Where `path` is in the recent projects (File ▸ Open Recent, Home's Recent list).
    fn recent(&mut self, path: &str) -> usize {
        self.app().session.prefs.general.recent_projects.iter().position(|p| p == path).unwrap()
    }
}

/// A folder for one test holding the demo project saved as `current.fcproj` and `other.fcproj`.
/// The session is left on `current.fcproj` with nothing unsaved.
fn projects(d: &mut Driver, test: &str) -> (PathBuf, String, String) {
    let dir = std::env::temp_dir().join(format!("fc-open-project-prompt-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let current = dir.join("current.fcproj").to_string_lossy().into_owned();
    let other = dir.join("other.fcproj").to_string_lossy().into_owned();
    d.exec("file.saveAs", json!({"path": other}));
    d.exec("file.saveAs", json!({"path": current}));
    assert!(!d.app().session.is_dirty());
    (dir, current, other)
}

#[test]
fn open_project_save_saves_the_changes_then_opens_the_chosen_project() {
    let mut d = Driver::demo();
    let (dir, current, other) = projects(&mut d, "save");
    let clip = d.edit();
    d.open_project(&other);
    d.click("openProject.save");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other));
    assert!(!d.app().session.is_dirty());
    // the change was written to the project it was made in
    d.exec("file.open", json!({"path": current}));
    assert_eq!(d.label(clip), Label::Yellow);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_failed_save_keeps_the_prompt_and_chosen_project() {
    let mut d = Driver::demo();
    let (dir, _, other) = projects(&mut d, "failedSave");
    let folder = dir.join("current-folder");
    std::fs::create_dir(&folder).unwrap();
    let current = folder.join("current.fcproj").to_string_lossy().into_owned();
    d.exec("file.saveAs", json!({"path": current}));
    let clip = d.edit();
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::write(&folder, b"not a folder").unwrap();

    d.open_project(&other);
    d.click("openProject.save");
    assert!(d.has("openProject.save"), "the prompt stays open after a failed save");
    assert_eq!(d.app().ui.clip_dialog.as_ref().unwrap().params["path"], json!(other));
    assert_eq!(d.path(), Some(current.clone()));
    assert!(d.app().session.is_dirty());
    assert_eq!(d.label(clip), Label::Yellow);
    assert!(d.app().ui.status.contains(&current), "the status shows the save error: {}", d.app().ui.status);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn close_project_and_quit_failed_save_keep_the_prompt() {
    for (command, prompt) in [("file.closeProject", "closeProject"), ("app.quit", "quit")] {
        let mut d = Driver::demo();
        let (dir, _, _) = projects(&mut d, prompt);
        let folder = dir.join("current-folder");
        std::fs::create_dir(&folder).unwrap();
        let current = folder.join("current.fcproj").to_string_lossy().into_owned();
        d.exec("file.saveAs", json!({"path": current}));
        let clip = d.edit();
        std::fs::remove_dir_all(&folder).unwrap();
        std::fs::write(&folder, b"not a folder").unwrap();

        if command == "app.quit" {
            d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
            d.frames(3);
        } else {
            d.menu(command);
        }
        let save = format!("{prompt}.save");
        assert!(d.has(&save), "{command} shows its save prompt");
        d.click(&save);
        assert!(d.has(&save), "{command} keeps its prompt after a failed save");
        assert_eq!(d.path(), Some(current.clone()));
        assert!(d.app().session.is_dirty());
        assert_eq!(d.label(clip), Label::Yellow, "{command} keeps the change");
        assert!(d.app().ui.status.contains(&current), "{command} shows the save error: {}", d.app().ui.status);
        assert!(!d.app().quit_confirmed, "{command} does not confirm a quit");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn home_recent_with_unsaved_changes_asks_first() {
    let mut d = Driver::demo();
    let (dir, current, other) = projects(&mut d, "homeRecent");
    let first = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let clip = d.edit();
    d.click("header.home");
    let i = d.recent(&other);
    d.click(&format!("import.recent.{i}"));
    assert_eq!(d.path(), Some(current.clone()), "nothing opens before an answer");
    assert_eq!(d.app().ui.mode, Mode::Import, "Home stays while it asks");
    // Cancel: Home stays, with this project and its changes
    d.click("openProject.cancel");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!((d.path(), d.app().ui.mode), (Some(current.clone()), Mode::Import));
    assert!(d.app().session.is_dirty());
    d.click(&format!("import.recent.{i}"));
    assert!(d.has("openProject.dontSave"));
    d.click("openProject.dontSave");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other));
    assert_eq!(d.app().ui.mode, Mode::Edit);
    assert!(!d.app().session.is_dirty());
    // the project the change was made in is as it was saved
    d.exec("file.open", json!({"path": current}));
    assert_eq!(d.label(clip), first.label);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn home_recent_save_on_a_never_saved_project_switches_to_edit_once_it_opens() {
    let mut d = Driver::demo();
    let (dir, _, other) = projects(&mut d, "homeNeverSaved");
    d.app().session.path = None;
    d.edit();
    let saved = dir.join("saved.fcproj").to_string_lossy().into_owned();
    d.app().hooks.pick_save = Some(Box::new(move |_| Some(saved.clone())));
    d.click("header.home");
    let i = d.recent(&other);
    d.click(&format!("import.recent.{i}"));
    assert_eq!(d.app().ui.mode, Mode::Import);
    d.click("openProject.save");
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other));
    assert_eq!(d.app().ui.mode, Mode::Edit);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_asks_when_the_open_dialog_answers_in_a_later_frame() {
    let mut d = Driver::demo();
    let (dir, current, other) = projects(&mut d, "runner");
    d.edit();
    // a host that shows file dialogs without blocking (the desktop app on Linux)
    let answer: Arc<Mutex<Option<Sender<Vec<String>>>>> = Arc::default();
    let a = answer.clone();
    d.app().hooks.run_dialog = Some(Box::new(move |_| {
        let (tx, rx) = channel();
        *a.lock().unwrap() = Some(tx);
        rx
    }));
    assert_eq!(d.menu("file.open"), json!({"dialog": "open"}));
    answer.lock().unwrap().take().unwrap().send(vec![other.clone()]).unwrap();
    d.frames(3);
    assert!(d.has("openProject.dontSave"));
    assert_eq!(d.path(), Some(current));
    d.click("openProject.dontSave");
    assert_eq!(d.path(), Some(other));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_project_save_waits_for_a_save_dialog_answered_later() {
    let mut d = Driver::demo();
    let (dir, _, other) = projects(&mut d, "saveLater");
    d.app().session.path = None;
    let clip = d.edit();
    let saved = dir.join("saved.fcproj").to_string_lossy().into_owned();
    // a host that shows file dialogs without blocking (the desktop app on Linux), answered by hand
    let asked: Arc<Mutex<Vec<(FileDialog, Sender<Vec<String>>)>>> = Arc::default();
    let a = asked.clone();
    d.app().hooks.run_dialog = Some(Box::new(move |dialog| {
        let (tx, rx) = channel();
        a.lock().unwrap().push((dialog, tx));
        rx
    }));
    let answer = |d: &mut Driver, paths: Vec<String>| {
        let (dialog, tx) = asked.lock().unwrap().remove(0);
        tx.send(paths).unwrap();
        d.frames(3);
        dialog
    };
    d.menu("file.open");
    assert!(matches!(answer(&mut d, vec![other.clone()]), FileDialog::OpenProject));
    // the Save dialog is canceled: back to the prompt, nothing opens
    d.click("openProject.save");
    assert!(matches!(answer(&mut d, Vec::new()), FileDialog::SaveProject { .. }));
    assert!(d.has("openProject.save"));
    assert_eq!(d.path(), None);
    // the save fails: the prompt stays, with the reason in the status line
    let blocker = dir.join("not-a-folder");
    std::fs::write(&blocker, b"").unwrap();
    d.click("openProject.save");
    answer(&mut d, vec![blocker.join("saved.fcproj").to_string_lossy().into_owned()]);
    assert!(d.has("openProject.save"));
    assert!(!d.app().ui.status.is_empty());
    assert_eq!(d.path(), None);
    assert!(d.app().session.is_dirty());
    // the file is picked a few frames later: saved, then the chosen project opens
    d.click("openProject.save");
    d.frames(3);
    assert_eq!(d.path(), None, "nothing opens before the answer");
    answer(&mut d, vec![saved.clone()]);
    assert!(d.app().ui.clip_dialog.is_none());
    assert_eq!(d.path(), Some(other.clone()));
    assert!(!d.app().session.is_dirty());
    d.exec("file.open", json!({"path": saved}));
    assert_eq!(d.label(clip), Label::Yellow, "the change was saved");
    // a new project changed while the Save dialog is open: the late answer saves and opens nothing
    let late = dir.join("late.fcproj").to_string_lossy().into_owned();
    d.app().session.path = None;
    d.exec("file.newBin", json!({"name": "Late"}));
    assert!(d.app().session.is_dirty());
    d.menu("file.open");
    answer(&mut d, vec![other]);
    d.click("openProject.save");
    d.exec("file.newProject", json!({"name": "New"}));
    d.exec("file.newBin", json!({"name": "Changed"}));
    answer(&mut d, vec![late.clone()]);
    assert!(!std::path::Path::new(&late).exists());
    assert_eq!(d.path(), None);
    assert!(d.app().session.is_dirty());
    assert!(d.app().ui.clip_dialog.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_save_prompt_closes_when_a_script_replaces_its_project() {
    let mut d = Driver::demo();
    let (dir, _, other) = projects(&mut d, "replaced");
    // Open Project
    d.edit();
    d.open_project(&other);
    assert!(d.has("openProject.dontSave"));
    d.exec("file.newProject", json!({"name": "New"}));
    d.exec("file.newBin", json!({"name": "Changed"}));
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(!d.has("openProject.dontSave"));
    assert_eq!(d.path(), None);
    assert!(d.app().session.is_dirty(), "the new project's change is kept");
    // Close Project
    d.menu("file.closeProject");
    assert!(d.has("closeProject.dontSave"));
    d.exec("file.newProject", json!({"name": "New"}));
    d.exec("file.newBin", json!({"name": "Changed"}));
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(d.app().session.is_dirty());
    // Quit
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    d.frames(3);
    assert!(d.has("quit.dontSave"));
    d.exec("file.newProject", json!({"name": "New"}));
    d.exec("file.newBin", json!({"name": "Changed"}));
    assert!(d.app().ui.clip_dialog.is_none());
    assert!(!d.app().quit_confirmed);
    assert!(d.app().session.is_dirty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn desktop_folder_menu_bin_close_and_standard_export() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ordinary.fcproj");
    let mut p = filmcraft_engine::project::Project::new("Editorial");
    let bin = p.add_bin("Daily", None);
    std::fs::write(&path, filmcraft_format::encode(&p, false)).unwrap();
    let folder = temp.path().join("editorial");
    filmcraft_format::project_folder::pack(&path, &folder, filmcraft_format::atomic_write_new).unwrap();
    let s = filmcraft_engine::Session::desktop_project_editor(filmcraft_engine::autosave::AutosaveConfig::new(temp.path().join("user"))).unwrap();
    let mut d = Driver::open(s, crate::support::with_shots(crate::support::builder()));
    let dir = folder.to_string_lossy().into_owned();
    d.app().hooks.pick_folder = Some(Box::new(move || Some(dir.clone())));
    d.menu("file.openProjectFolder");
    assert!(d.app().session.editing_bins());
    d.ok("ui.menu.invoke", json!({"id":"projectPanel.openBin","params":{"how":"tab","bin":bin.0}}));
    d.frames(3);
    use egui_kittest::kittest::Queryable;
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::Button, "Close Bin");
    crate::support::save_shot(&mut d, 2, "desktop-bin.png");
    assert_eq!(d.app().session.bin_access(bin.0), Some(true));
    d.exec("project.renameBin", json!({"bin":bin.0,"name":"Edited"}));
    let dest = temp.path().join("copy.fcproj");
    let filename = dest.to_string_lossy().into_owned();
    d.app().hooks.pick_save = Some(Box::new(move |_| Some(filename.clone())));
    d.menu("file.exportStandardProject");
    assert_eq!(filmcraft_format::decode(&std::fs::read(dest).unwrap()).unwrap().project.root.find_bin(bin).unwrap().name, "Edited");
    d.click("projectBin.0.closeBin");
    assert!(d.app().session.bin_access(bin.0).is_none());
    let flat = temp.path().join("stock.fcproj");
    filmcraft_format::project_folder::unpack(&folder, &flat).unwrap();
    assert_eq!(filmcraft_format::decode(&std::fs::read(flat).unwrap()).unwrap().project.root.find_bin(bin).unwrap().name, "Edited");
}

#[test]
fn desktop_recovery_selects_a_bin_and_exports_while_another_editor_owns_it() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = filmcraft_engine::autosave::AutosaveConfig::new(temp.path().join("user"));
    let src = temp.path().join("ordinary.fcproj");
    let folder = temp.path().join("cut");
    let mut project = filmcraft_project::Project::new("Rushes");
    let a = project.add_bin("First", None);
    let b = project.add_bin("Second", None);
    std::fs::write(&src, filmcraft_format::encode(&project, false)).unwrap();
    filmcraft_format::project_folder::pack(&src, &folder, filmcraft_format::atomic_write_new).unwrap();
    let manifest = folder.join("project.fcmod");
    let mut abandoned = filmcraft_engine::Session::desktop_project_editor(cfg.clone()).unwrap();
    abandoned.execute("file.open", json!({"path":manifest})).unwrap();
    for bin in [a, b] {
        abandoned.execute("project.renameBin", json!({"bin":bin.0,"name":format!("Draft {}",bin.0)})).unwrap();
        abandoned.flush_folder_recovery().unwrap();
    }
    drop(abandoned);
    let mut peer = filmcraft_engine::Session::desktop_project_editor(cfg.clone()).unwrap();
    peer.execute("file.open", json!({"path":manifest})).unwrap();
    peer.execute("project.checkoutBin", json!({"bin":a.0})).unwrap();
    let mut next = filmcraft_engine::Session::desktop_project_editor(cfg).unwrap();
    let rows = next.execute("file.recoveryList", json!({})).unwrap();
    let i = rows.as_array().unwrap().iter().position(|r| r["bin"] == a.0).unwrap();
    let mut d = Driver::open(next, crate::support::with_shots(crate::support::builder()));
    d.click(&format!("recovery.item.{i}"));
    use egui_kittest::kittest::Queryable;
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::Button, "Recover to File…");
    crate::support::save_shot(&mut d, 2, "bin-recovery.png");
    d.click("recovery.recover");
    assert!(d.app().ui.status.contains("busy"), "{}", d.app().ui.status);
    let out = temp.path().join("rescued.fcproj");
    let path = out.to_string_lossy().to_string();
    d.app().hooks.pick_save = Some(Box::new(move |_| Some(path.clone())));
    d.click("recovery.export");
    let saved = filmcraft_format::decode(&std::fs::read(out).unwrap()).unwrap().project;
    assert_eq!(saved.root.find_bin(a).unwrap().name, format!("Draft {}", a.0));
    let rows = d.exec("file.recoveryList", json!({}));
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["bin"], b.0);
    d.click("recovery.recover");
    assert_eq!(d.app().session.bin_access(b.0), Some(true));
}
