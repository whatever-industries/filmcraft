//! The project tab displays the folder directory. Opening an editorial bin uses
//! the same single-view lifecycle as opening it from project search.
use super::{Actions, FilmcraftApp, Inst, ProjectDialog, open_bin};
use filmcraft_project::{BinId, DirectoryEntry};
use serde_json::json;

pub(super) fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, actions: &mut Actions) {
    let directory = app.session.project.directory().into_owned();
    let selected = app.ui.project_panel.folder.filter(|id| directory.folders.contains_key(id)).unwrap_or(directory.root);
    ui.horizontal(|ui| {
        for (label, command) in [(tl!("New Folder"), "file.newFolder"), (tl!("New Bin"), "file.newBin")] {
            let r = ui.button(label);
            app.auto.add_response(command, &r, label);
            if r.clicked() {
                app.ui.project_panel.dialog = Some(ProjectDialog::DirectoryName { command: command.into(), parent: selected, name: String::new() });
            }
        }
        if ui.button(tl!("Rename Folder")).clicked()
            && let Some(folder) = directory.folders.get(&selected)
        {
            app.ui.project_panel.dialog =
                Some(ProjectDialog::DirectoryName { command: "project.renameFolder".into(), parent: selected, name: folder.name.clone() });
        }
    });
    egui::ScrollArea::vertical().show(ui, |ui| {
        let mut pending = vec![(DirectoryEntry::Folder(directory.root), 0)];
        while let Some((entry, depth)) = pending.pop() {
            let (id, name, is_folder) = match entry {
                DirectoryEntry::Folder(id) => {
                    let Some(f) = directory.folders.get(&id) else { continue };
                    if !app.ui.project_panel.collapsed_folders.contains(&id) {
                        pending.extend(f.children.iter().rev().map(|e| (*e, depth + 1)));
                    }
                    (id, f.name.clone(), true)
                }
                DirectoryEntry::Bin(id) => {
                    let Some(b) = app.session.project.root.find_bin(id) else { continue };
                    (id, b.name.clone(), false)
                }
            };
            let label = if is_folder { tlf!("Folder {name}", name) } else { tlf!("Bin {name}", name) };
            ui.push_id((is_folder, id.0), |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * ui.spacing().indent);
                    if is_folder {
                        let closed = app.ui.project_panel.collapsed_folders.contains(&id);
                        let label = if closed { tlf!("Expand {name}", name) } else { tlf!("Collapse {name}", name) };
                        let toggle = crate::widgets::accessible_name(ui.small_button(if closed { "▸" } else { "▾" }), label);
                        app.auto.add_response(
                            &format!("project.folder.{}.toggle", id.0),
                            &toggle,
                            if closed { tl!("Expand folder") } else { tl!("Collapse folder") },
                        );
                        if toggle.clicked() && !app.ui.project_panel.collapsed_folders.remove(&id) {
                            app.ui.project_panel.collapsed_folders.insert(id);
                        }
                    }
                    let r = ui.add(egui::Button::new(&label).selected(is_folder && id == selected).sense(egui::Sense::click_and_drag()));
                    let kind = if is_folder { "folder" } else { "bin" };
                    app.auto.add_response(&format!("project.{kind}.{}", id.0), &r, &label);
                    if !is_folder {
                        crate::panels::project_views::accept_bin_drop(app, ui, r.rect, id.0, actions);
                    }
                    if is_folder
                        && ui.rect_contains_pointer(r.rect)
                        && ui.input(|i| i.pointer.any_released())
                        && let Some(item) = crate::panels::dragged_project_item(ui)
                    {
                        let selection = &app.session.state.project_selection;
                        let items = if selection.contains(&item) { selection.clone() } else { vec![item] };
                        crate::panels::clear_drag(ui);
                        app.ui.project_panel.dialog = Some(ProjectDialog::ChooseBin {
                            command: "project.moveToBin".into(),
                            params: json!({"items":items}),
                            parent: id,
                            name: String::new(),
                        });
                    }
                    r.dnd_set_drag_payload(entry);
                    if is_folder && let Some(moving) = r.dnd_release_payload::<DirectoryEntry>() {
                        actions.push(("project.moveToFolder".into(), json!({"folder":id,"entry":*moving})));
                    }
                    if r.clicked() {
                        r.request_focus();
                        if is_folder {
                            super::bin_views::focus(app, None);
                            app.ui.project_panel.folder = Some(id);
                        } else if let Err(e) = open_bin(app, Inst::Main, id.0, Some("openNewTab"), ui.input(|i| i.modifiers)) {
                            app.ui.status = e;
                        }
                    }
                    if entry != DirectoryEntry::Folder(directory.root) {
                        r.context_menu(|ui| {
                            ui.menu_button(tl!("Move to Folder"), |ui| {
                                for (&dest, folder) in &directory.folders {
                                    if ui.button(&folder.name).clicked() {
                                        actions.push(("project.moveToFolder".into(), json!({"folder":dest,"entry":entry})));
                                        ui.close();
                                    }
                                }
                            });
                        });
                    }
                })
            });
        }
    });
}

pub(crate) fn selected_folder(app: &FilmcraftApp) -> BinId {
    let dir = app.session.project.directory();
    app.ui.project_panel.folder.filter(|id| dir.folders.contains_key(id)).unwrap_or(dir.root)
}

pub(crate) fn choose_destination(app: &mut FilmcraftApp, command: &str, params: &mut serde_json::Value) -> bool {
    if app.engine_call || app.session.folder_revision().is_none() || !matches!(command, "file.import" | "file.newSequence") || !params["bin"].is_null() {
        return false;
    }
    let inst = super::destination_inst(app);
    if inst != Inst::Main {
        params["bin"] = json!(super::view_of(app, inst).bin);
        return false;
    }
    let parent = selected_folder(app);
    app.ui.project_panel.dialog = Some(ProjectDialog::ChooseBin { command: command.into(), params: params.take(), parent, name: String::new() });
    true
}
