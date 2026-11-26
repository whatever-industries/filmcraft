//! The crash-recovery prompt, the Revert confirmation and the replace prompt for a save dialog's
//! answer that got its extension added (`native_dialogs`). Settings ▸ Auto Save lives in
//! `panels::settings`.
//!
//! Automation ids: `recovery.item.<n>`, `recovery.recover`, `recovery.discard`, `recovery.later`;
//! `revert.yes`, `revert.no`; `overwrite.replace`, `overwrite.cancel`.

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, vec2};
use serde_json::json;

use crate::theme::Tokens;
use crate::{Dialog, FilmcraftApp};

/// Dialog-local state.
#[derive(Default)]
pub struct FileDialogState {
    entries: Vec<serde_json::Value>,
    recovery_choice: Option<usize>,
    recovery_error: String,
}

impl FileDialogState {
    pub(crate) fn refresh(&mut self, session: &mut filmcraft_engine::Session) -> Result<bool, String> {
        let rows = session.execute("file.recoveryList", json!({})).map_err(|e| e.to_string())?;
        self.entries = rows.as_array().cloned().unwrap_or_default();
        self.recovery_choice = (self.entries.len() == 1).then_some(0);
        self.recovery_error.clear();
        Ok(!self.entries.is_empty())
    }
}

pub(crate) fn modal_frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0))
}

pub(crate) fn button(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, primary: bool) -> bool {
    let t = app.tokens;
    let text = RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text });
    let b = egui::Button::new(text)
        .min_size(vec2(88.0, 30.0))
        .corner_radius(CornerRadius::same(15))
        .fill(if primary { t.accent } else { Color32::TRANSPARENT })
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.5, t.text_faint) });
    let r = ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand);
    app.auto.add(id, r.rect, label);
    r.clicked()
}

fn recovery_key(row: &serde_json::Value) -> serde_json::Value {
    let mut key = json!({"id":row["id"]});
    if row["storage"].is_string() {
        key["storage"] = row["storage"].clone();
    }
    key
}

fn recovery_label(row: &serde_json::Value) -> String {
    let name = row["projectName"].as_str().unwrap_or(tl!("Untitled"));
    let kind = if let Some(bin) = row["bin"].as_u64() {
        if let Some(label) = row["binName"].as_str() { format!("{label} (#{bin})") } else { tlf!("Bin {bin}", bin) }
    } else if row["storage"] == "projectFolder" {
        tl!("Project Folder").to_string()
    } else {
        tl!("Project File").to_string()
    };
    format!("{name} — {kind} — {}", row["savedAt"].as_str().unwrap_or(""))
}

pub fn show_recovery(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let items = app.file_dialogs.entries.clone();
    if items.is_empty() {
        return false;
    }
    let mut choice = app.file_dialogs.recovery_choice;
    let mut refresh = false;
    let mut open = true;
    egui::Modal::new(egui::Id::unique("recovery-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(520.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Recover Unsaved Changes")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            if let Some(c) = choice.and_then(|i| items.get(i)) {
                ui.label(recovery_label(c));
                if let Some(path) = c["projectPath"].as_str() {
                    ui.label(path);
                }
                if let Some(e) = c["error"].as_str() {
                    ui.label(e);
                }
            }
            if items.len() > 1 {
                ui.add_space(8.0);
                ui.label(RichText::new(tlf!("{n} sessions have unsaved changes:", n = items.len())).size(12.0).color(t.text_dim));
                for (i, it) in items.iter().enumerate() {
                    let label = recovery_label(it);
                    let r = ui.radio(choice == Some(i), RichText::new(&label).size(12.5));
                    app.auto.add(&format!("recovery.item.{i}"), r.rect, &label);
                    if r.clicked() {
                        choice = Some(i);
                        app.file_dialogs.recovery_choice = choice;
                        app.file_dialogs.recovery_error.clear();
                    }
                }
            }
            if !app.file_dialogs.recovery_error.is_empty() {
                ui.label(&app.file_dialogs.recovery_error);
            }
            ui.add_space(18.0);
            let selected = choice.and_then(|i| items.get(i)).cloned();
            ui.horizontal(|ui| {
                ui.add_enabled_ui(selected.is_some(), |ui| {
                    if button(app, ui, "recovery.discard", tl!("Discard"), false)
                        && let Some(row) = &selected
                    {
                        match app.session.execute("file.discardRecovery", recovery_key(row)) {
                            Ok(_) => refresh = true,
                            Err(e) => {
                                app.ui.status = e.to_string();
                                app.file_dialogs.recovery_error = app.ui.status.clone();
                            }
                        }
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(selected.is_some(), |ui| {
                        if button(app, ui, "recovery.recover", tl!("Recover"), true)
                            && let Some(row) = &selected
                        {
                            match app.session.execute("file.recover", recovery_key(row)) {
                                Ok(result) => {
                                    if let Some(bin) = result["bin"].as_u64() {
                                        app.ui.project_panel.bin = Some(bin);
                                        app.ui.project_panel.active_tab = None;
                                    }
                                    refresh = true;
                                }
                                Err(e) => {
                                    app.ui.status = e.to_string();
                                    app.file_dialogs.recovery_error = app.ui.status.clone();
                                }
                            }
                        }
                    });
                    if let Some(row) = &selected
                        && row["storage"] == "projectFolder"
                        && button(app, ui, "recovery.export", tl!("Recover to File…"), false)
                    {
                        let mut key = recovery_key(row);
                        if let Err(e) = app.pick(crate::native_dialogs::FileDialog::SaveProject { name: "Recovered.fcproj".into() }, move |app, paths| {
                            let Some(path) = paths.into_iter().next() else { return Ok(serde_json::Value::Null) };
                            key["path"] = path.into();
                            let result = app.session.execute("file.recover", key).map_err(|e| {
                                app.file_dialogs.recovery_error = e.to_string();
                                e.to_string()
                            })?;
                            app.file_dialogs.refresh(&mut app.session)?;
                            Ok(result)
                        }) {
                            app.ui.status = e;
                        }
                    }
                    if button(app, ui, "recovery.later", tl!("Not Now"), false) {
                        open = false;
                    }
                });
            });
        });
    });
    if refresh {
        match app.file_dialogs.refresh(&mut app.session) {
            Ok(any) => return open && any,
            Err(e) => app.ui.status = e,
        }
    }
    open
}

pub fn show_revert(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let file = app.session.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut open = true;
    let r = egui::Modal::new(egui::Id::unique("revert-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(460.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Revert")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(tlf!("Are you sure you want to discard your changes to '{file}'?", file)).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(app, ui, "revert.yes", tl!("Yes"), true) {
                    if let Err(e) = app.session.execute("file.revert", json!({})) {
                        app.ui.status = e.to_string();
                    }
                    open = false;
                }
                if button(app, ui, "revert.no", tl!("No"), false) {
                    open = false;
                }
            });
        });
    });
    // (the modal's own close also fires on Escape: not while a drag is being cancelled, #580)
    // The runtime takes Escape from egui as the window's cancel, so the modal does not see it.
    open && (!(r.should_close() || crate::framework::escape_pressed(ctx)) || ctx.input(|i| i.pointer.any_down()))
}

/// "“cut.fcproj” already exists in “edits”. Do you want to replace it?" for a save dialog's answer
/// that names an existing file once its extension was added (drawn after every other dialog, so
/// it stays on top). Escape cancels.
pub fn show_overwrite(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(path) = app.overwrite_prompt_path().map(std::path::PathBuf::from) else { return };
    let t = app.tokens;
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = path.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut answer = None;
    let r = egui::Modal::new(egui::Id::unique("overwrite-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(460.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Replace File")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(tlf!("“{file}” already exists in “{folder}”. Do you want to replace it?", file, folder)).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(app, ui, "overwrite.replace", tl!("Replace"), true) {
                    answer = Some(true);
                }
                if button(app, ui, "overwrite.cancel", tl!("Cancel"), false) {
                    answer = Some(false);
                }
            });
        });
    });
    // (the modal's own close also fires on Escape: not while a drag is being cancelled, #580)
    if answer.is_none() && (r.should_close() || crate::framework::escape_pressed(ctx)) && !ctx.input(|i| i.pointer.any_down()) {
        answer = Some(false);
    }
    if let Some(replace) = answer {
        app.answer_overwrite(replace);
    }
}

/// Draw the dialog if it is one of ours; returns Some(still open).
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context, d: Dialog) -> Option<bool> {
    match d {
        Dialog::Preferences => Some(crate::panels::settings::show(app, ctx)),
        Dialog::Recovery => Some(show_recovery(app, ctx)),
        Dialog::RevertConfirm => Some(show_revert(app, ctx)),
        _ => None,
    }
}
