use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use filmcraft_engine::{Session, autosave::AutosaveConfig};
use filmcraft_format::project_folder::{FolderSnapshot, pack};
use std::{sync::mpsc, time::Duration};

#[test]
fn pending_comparison_allows_back_close_and_switching_bins() {
    let temp = tempfile::tempdir().unwrap();
    let mut project = filmcraft_format::decode(include_bytes!("../../../../format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let first = project.add_bin("First bin", None);
    let second = project.add_bin("Second bin", None);
    let stock = temp.path().join("source.fcproj");
    let folder = temp.path().join("folder");
    std::fs::write(&stock, filmcraft_format::encode(&project, false)).unwrap();
    pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let path = folder.join("project.fcmod");
    let (mut snapshot, before) = FolderSnapshot::open(&path).unwrap();
    project.root.find_bin_mut(first).unwrap().name = "New first".into();
    project.root.find_bin_mut(second).unwrap().name = "New second".into();
    snapshot.save(&before, &filmcraft_format::encode(&project, false)).unwrap();
    drop(snapshot);
    let mut session = Session::desktop_project_editor(AutosaveConfig::new(temp.path().join("profile"))).unwrap();
    session.execute("file.open", json!({"path":path})).unwrap();
    let mut app = FilmcraftApp::new(session);
    open(&mut app, Some(first.0));
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
    // Occupy the worker until we have exercised the real navigation controls.
    let (release, gate) = mpsc::channel();
    let ctx = h.ctx.clone();
    let _blocker = h
        .state_mut()
        .bin_history_worker
        .submit(&ctx, move || {
            gate.recv_timeout(Duration::from_secs(60)).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        })
        .unwrap();

    for exit in ["Back to Bin History", "Close"] {
        h.run_steps(3);
        h.get_by_label("Revision 1").click();
        h.run_steps(3);
        h.get_by_label("Compare Versions").click();
        h.run_steps(3);
        assert!(h.query_by_label("Comparing saved versions…").is_some());
        assert!(h.query_by_label("Copy from first save").is_none());
        h.get_by_label(exit).click();
        h.run_steps(3);
        assert!(h.query_by_label("Comparing saved versions…").is_none());
        if exit == "Back to Bin History" {
            h.get_by_label("Revisions").click();
        }
    }
    assert!(h.state().ui.project_panel.history_browser.is_none());
    open(h.state_mut(), Some(second.0));
    h.run_steps(3);
    h.get_by_label("Revision 1").click();
    h.run_steps(3);
    h.get_by_label("Compare Versions").click();
    h.run_steps(3);
    assert!(h.query_by_label("Comparing saved versions…").is_some());
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while h.query_by_label("Comparing saved versions…").is_some() {
        assert!(std::time::Instant::now() < deadline);
        h.run_steps(1);
        std::thread::yield_now();
    }
    assert!(h.query_by_label("To · 1 New second").is_some());
    assert!(h.query_by_label("To · 1 New first").is_none());
    assert!(h.state().ui.project_panel.history_clipboard.is_none());
}

#[test]
fn project_history_runs_retention_preview_with_the_open_reader() {
    let temp = tempfile::tempdir().unwrap();
    let stock = temp.path().join("source.fcproj");
    let folder = temp.path().join("folder");
    std::fs::write(&stock, include_bytes!("../../../../format/tests/fixtures/v1-edit.fcproj")).unwrap();
    pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let mut session = Session::desktop_project_editor(AutosaveConfig::new(temp.path().join("profile"))).unwrap();
    session.execute("file.open", json!({"path":folder.join("project.fcmod")})).unwrap();
    let mut app = FilmcraftApp::new(session);
    open(&mut app, None);
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
    h.run_steps(3);
    h.get_by_label("Preview History Retention").click();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while h.query_by_label("Confirmed reclaimable").is_none() {
        assert!(std::time::Instant::now() < deadline);
        h.run_steps(1);
        std::thread::yield_now();
    }
    h.get_by_label("Back to Project History").click();
    h.run_steps(3);
    assert!(h.query_by_label("Preview History Retention").is_some());
    assert!(h.query_by_label("Confirmed reclaimable").is_none());
}

#[test]
fn milestone_labels_open_the_selected_save_and_damaged_labels_leave_history_browsable() {
    use filmcraft_format::project_folder::pins;
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("folder");
    let stock = temp.path().join("edit.fcproj");
    let mut project = filmcraft_format::decode(include_bytes!("../../../../format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let bin = project.add_bin("First edit", None);
    std::fs::write(&stock, filmcraft_format::encode(&project, false)).unwrap();
    pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let path = folder.join("project.fcmod");
    let (mut snapshot, before) = FolderSnapshot::open(&path).unwrap();
    let older = snapshot.revision().to_owned();
    pins::pin(&folder, &older, "Screening").unwrap();
    project.root.find_bin_mut(bin).unwrap().name = "Second edit".into();
    snapshot.save(&before, &filmcraft_format::encode(&project, false)).unwrap();
    pins::pin(&folder, snapshot.revision(), "Screening").unwrap();
    drop(snapshot);

    let mut session = Session::desktop_project_editor(AutosaveConfig::new(temp.path().join("profile"))).unwrap();
    session.execute("file.open", json!({"path":path})).unwrap();
    let mut app = FilmcraftApp::new(session);
    open(&mut app, None);
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
    h.run_steps(3);
    h.get_by_label("Revision 0 · Screening").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("First edit  #").is_some());
    h.get_by_label("Revisions").click();
    h.run_steps(3);
    h.get_by_label("Revision 1 · Screening").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("Second edit  #").is_some());

    pins::rename(&folder, &older, "Screening", "Rough cut").unwrap();
    open(h.state_mut(), Some(bin.0));
    h.run_steps(3);
    h.get_by_label("Revision 0 · Rough cut").click();
    h.run_steps(3);
    assert!(h.query_by_label("First edit").is_some());
    std::fs::write(folder.join("revision-pins.json"), b"broken").unwrap();
    h.get_by_label("Revisions").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("Milestone labels unavailable").is_some());
    h.get_by_label("Revision 1").click();
    h.run_steps(3);
    assert!(h.query_by_label("Second edit").is_some());
}
