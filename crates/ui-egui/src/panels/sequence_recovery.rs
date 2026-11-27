//! Browse retained saves and move backups without changing the open timelines.
use crate::{FilmcraftApp, native_dialogs::FileDialog};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize)]
struct Move {
    sequence: u64,
    snapshot: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Save {
    number: u64,
    saved_at: Option<String>,
    revision: Option<String>,
    moves: Vec<Move>,
}

#[derive(Clone, Debug, Deserialize)]
struct Page {
    versions: Vec<Save>,
    before: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Draft {
    project: Arc<()>,
    return_focus: Option<egui::Id>,
    focus_first: bool,
    page: Page,
    cursors: Vec<u64>,
    version: Option<String>,
    row: Option<(u64, Option<u64>)>,
    sequences: Vec<(u64, String)>,
    sequence: Option<u64>,
    message: String,
}

fn page(app: &mut FilmcraftApp, before: Option<u64>) -> Result<Page, String> {
    let params = before.map_or_else(|| json!({}), |n| json!({"before":n}));
    let result = app.session.execute("project.recoveryVersions", params).map_err(|e| e.to_string())?;
    serde_json::from_value(result).map_err(|e| e.to_string())
}

pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context) -> Result<Value, String> {
    app.ui.sequence_recovery = Some(Draft {
        page: page(app, None)?,
        project: app.session.project_instance.clone(),
        return_focus: ctx.memory(|m| m.focused()),
        focus_first: true,
        cursors: Vec::new(),
        version: None,
        row: None,
        sequences: Vec::new(),
        sequence: None,
        message: String::new(),
    });
    Ok(json!({"dialog":"sequenceRecovery"}))
}

impl Draft {
    fn select(&mut self, app: &mut FilmcraftApp, hash: &str, moved: Option<u64>) -> Result<(), String> {
        self.version = None;
        self.sequence = None;
        self.sequences.clear();
        self.message.clear();
        let mut params = json!({"revision":hash});
        if let Some(id) = moved {
            params["sequence"] = json!(id);
        }
        let result = app.session.execute("project.recoverySequences", params).map_err(|e| e.to_string())?;
        self.sequences = serde_json::from_value(result["sequences"].clone()).map_err(|e| e.to_string())?;
        self.sequence = self.sequences.first().map(|(id, _)| *id);
        self.version = Some(hash.to_owned());
        if self.sequences.is_empty() {
            self.message = tl!("This backup contains no sequences.").into();
        }
        Ok(())
    }
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.sequence_recovery.take() else { return };
    if !Arc::ptr_eq(&d.project, &app.session.project_instance) {
        return;
    }
    let title = tl!("Restore Sequence from Backup");
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("sequence-recovery"))
        .frame(super::file_dialogs::modal_frame(&app.tokens).inner_margin(egui::Margin::same(18)))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(title);
            show_versions(app, ui, &mut d);
            if !d.sequences.is_empty() {
                ui.separator();
                ui.label(tl!("Sequences"));
                egui::ScrollArea::vertical().id_salt("backup-sequences").max_height(140.0).show(ui, |ui| {
                    for (id, name) in &d.sequences {
                        let r = ui.selectable_label(d.sequence == Some(*id), name);
                        app.auto.add_response(&format!("sequenceRecovery.sequence.{id}"), &r, name);
                        if r.clicked() {
                            d.sequence = Some(*id);
                            d.message.clear();
                        }
                    }
                });
            }
            if !d.message.is_empty() {
                let r = ui.label(&d.message);
                ui.ctx().accesskit_node_builder(r.id, |n| n.set_live(egui::accesskit::Live::Polite));
            }
            ui.separator();
            close = show_actions(app, ui, &mut d);
        });
    ctx.accesskit_node_builder(modal.response.id, |n| {
        n.set_role(egui::accesskit::Role::Dialog);
        n.set_label(title);
    });
    if close || modal.should_close() || crate::widgets::escape_closes(ctx) {
        if let Some(id) = d.return_focus {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    } else {
        app.ui.sequence_recovery = Some(d);
    }
}

fn show_versions(app: &mut FilmcraftApp, ui: &mut egui::Ui, d: &mut Draft) {
    ui.label(tl!("Saved versions"));
    let mut selection = None;
    egui::ScrollArea::vertical().id_salt("backup-versions").max_height(150.0).show(ui, |ui| {
        for save in &d.page.versions {
            let base = if save.number == 0 { tl!("Original project").to_owned() } else { tlf!("Save {number}", number = save.number) };
            let base = match &save.saved_at {
                Some(date) => format!("{base} · {date}"),
                None => base,
            };
            let versions = save.revision.iter().map(|h| (h, None, base.clone())).chain(save.moves.iter().map(|m| {
                let cut = app
                    .session
                    .project
                    .item(filmcraft_project::ItemId(m.sequence))
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| tlf!("Sequence {id}", id = m.sequence));
                (&m.snapshot, Some(m.sequence), tlf!("{save} · move backup for {cut}", save = base, cut))
            }));
            for (hash, moved, label) in versions {
                let r = ui.selectable_label(d.row == Some((save.number, moved)), &label);
                app.auto.add_response(
                    &format!("sequenceRecovery.version.{}.{}", save.number, moved.map_or_else(|| "save".into(), |n| n.to_string())),
                    &r,
                    &label,
                );
                if std::mem::take(&mut d.focus_first) {
                    r.request_focus();
                }
                if r.clicked() {
                    selection = Some((hash.clone(), moved, save.number));
                }
            }
        }
    });
    let mut cursors = d.cursors.clone();
    if d.page.before.is_some() || !d.cursors.is_empty() {
        ui.horizontal(|ui| {
            let newer = ui.add_enabled(!d.cursors.is_empty(), egui::Button::new(tl!("Newer saves")));
            app.auto.add_response("sequenceRecovery.newer", &newer, tl!("Newer saves"));
            if newer.clicked() {
                cursors.pop();
            }
            let older = ui.add_enabled(d.page.before.is_some(), egui::Button::new(tl!("Older saves")));
            app.auto.add_response("sequenceRecovery.older", &older, tl!("Older saves"));
            if older.clicked()
                && let Some(before) = d.page.before
            {
                cursors.push(before);
            }
        });
    }
    if cursors != d.cursors {
        match page(app, cursors.last().copied()) {
            Ok(p) => {
                d.page = p;
                d.cursors = cursors;
                d.sequences.clear();
                d.version = None;
                d.row = None;
                d.sequence = None;
                d.message.clear();
                d.focus_first = true;
            }
            Err(e) => d.message = e,
        }
    }
    if let Some((hash, moved, n)) = selection {
        d.row = Some((n, moved));
        if let Err(e) = d.select(app, &hash, moved) {
            d.message = e;
        }
    }
}

fn show_actions(app: &mut FilmcraftApp, ui: &mut egui::Ui, d: &mut Draft) -> bool {
    let mut close = false;
    ui.horizontal_wrapped(|ui| {
        let params = json!({"revision":d.version,"sequence":d.sequence});
        let restore = ui.add_enabled(d.sequence.is_some(), egui::Button::new(tl!("Restore as Copy")));
        app.auto.add_response("sequenceRecovery.restore", &restore, tl!("Restore as Copy"));
        if restore.clicked() {
            match app.session.execute("project.restoreSequenceCopy", params.clone()) {
                Ok(r) if r["restored"] == true => {
                    app.session.toast(tlf!("Restored {name}", name = r["name"].as_str().unwrap_or_default()));
                    close = true;
                }
                Ok(r) => d.message = tlf!("Use Export Separate Project. {reason}", reason = r["reason"].as_str().unwrap_or_default()),
                Err(e) => d.message = e.to_string(),
            }
        }
        let export = ui.add_enabled(d.sequence.is_some(), egui::Button::new(tl!("Export Separate Project…")));
        app.auto.add_response("sequenceRecovery.export", &export, tl!("Export Separate Project…"));
        if export.clicked() {
            let owner = d.project.clone();
            app.pick_ui(FileDialog::save_as(tl!("FilmCraft Project"), &["fcproj"], "Recovered.fcproj"), move |app, paths| {
                let Some(path) = paths.into_iter().next() else { return };
                if !Arc::ptr_eq(&owner, &app.session.project_instance) {
                    return;
                }
                let mut params = params;
                params["path"] = json!(path);
                match app.session.execute("project.exportRecoverySequence", params) {
                    Ok(_) => app.session.toast(tlf!("Exported {path}", path)),
                    Err(e) => app.session.error_toast("project.exportRecoverySequence", e.to_string()),
                }
            });
        }
        let r = ui.button(tl!("Close"));
        app.auto.add_response("sequenceRecovery.close", &r, tl!("Close"));
        close |= r.clicked();
    });
    close
}
