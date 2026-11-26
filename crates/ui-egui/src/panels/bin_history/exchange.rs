//! Browse another modular project's saved bins without opening it as the live document.
use super::{button, worker::Pending};
use crate::{FilmcraftApp, native_dialogs::FileDialog};
use filmcraft_project::BinId;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
enum Selection {
    Saves,
    Bins(String),
    Preview { revision: String, bin: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Browser {
    epoch: u64,
    destination: u64,
    folder: String,
    selection: Selection,
    page: Result<Value, String>,
    pending: Option<Pending>,
}

impl Browser {
    fn params(&self) -> Value {
        let mut p = json!({"folder":self.folder});
        match &self.selection {
            Selection::Saves => {}
            Selection::Bins(rev) => p["revision"] = rev.as_str().into(),
            Selection::Preview { revision, bin } => {
                p["revision"] = revision.as_str().into();
                p["sourceBin"] = (*bin).into();
            }
        }
        p
    }

    fn read(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context, selection: Selection) {
        self.selection = selection;
        self.page = Ok(Value::Null);
        let read = app.session.prepare_external_bin_preview(&self.params()).map_err(|e| e.to_string());
        self.pending = match read.and_then(|job| app.bin_history_worker.submit(ctx, move || job().map_err(|e| e.to_string()))) {
            Ok(pending) => Some(pending),
            Err(error) => {
                self.page = Err(error);
                None
            }
        };
    }
}

pub(in crate::panels) fn menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, bin: u64) -> bool {
    if !app.session.is_modular_document() {
        return false;
    }
    let label = tl!("Import Saved Bin…");
    let r = ui
        .add_enabled(app.session.can_import_into_bin(bin), egui::Button::new(label))
        .on_disabled_hover_text(tl!("Open the destination bin for writing first"));
    app.auto.add("project.importSavedBin", r.rect, label);
    if r.clicked() {
        let epoch = app.session.document_epoch;
        let ctx = ui.ctx().clone();
        app.pick_ui(FileDialog::Folder { at: None }, move |app, paths| {
            if let Some(folder) = paths.into_iter().next()
                && app.session.document_epoch == epoch
                && app.session.is_modular_document()
            {
                let mut b = Browser { epoch, destination: bin, folder, selection: Selection::Saves, page: Ok(Value::Null), pending: None };
                b.read(app, &ctx, Selection::Saves);
                app.ui.project_panel.external_import = Some(b);
            }
        });
        return true;
    }
    false
}

fn notice(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, text: &str) {
    let r = ui.label(text);
    ui.ctx().accesskit_node_builder(r.id, |node| node.set_live(egui::accesskit::Live::Polite));
    app.auto.add(id, r.rect, text);
}

fn browse_page(b: &Browser, app: &mut FilmcraftApp, ui: &mut egui::Ui) -> Option<Selection> {
    if b.pending.is_some() {
        notice(app, ui, "binImport.loading", tl!("Reading saved project…"));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        return None;
    }
    let page = match &b.page {
        Ok(page) => page,
        Err(error) => {
            notice(app, ui, "binImport.error", error);
            return button(app, ui, "binImport.retry", tl!("Refresh")).then(|| b.selection.clone());
        }
    };
    match &b.selection {
        Selection::Saves => {
            ui.strong(tl!("Choose a saved revision"));
            for (i, row) in page["revisions"].as_array().into_iter().flatten().enumerate() {
                if let (Some(number), Some(hash)) = (row[0].as_u64(), row[1].as_str()) {
                    let label = format!("{} {} · {}", tl!("Revision"), number, hash.get(..8).unwrap_or(hash));
                    if button(app, ui, &format!("binImport.revision.{i}"), &label) {
                        return Some(Selection::Bins(hash.to_owned()));
                    }
                }
            }
        }
        Selection::Bins(rev) => {
            ui.strong(tl!("Choose a bin to preview"));
            if page["incomplete"] == true {
                notice(app, ui, "binImport.incomplete", tl!("Some bins or paths could not be read"));
                for error in page["errors"].as_object().into_iter().flat_map(|e| e.values()).filter_map(Value::as_str) {
                    ui.label(error);
                }
            }
            for row in page["bins"].as_array().into_iter().flatten() {
                let Some(id) = row["id"].as_u64() else { continue };
                let path = row["path"]
                    .as_array()
                    .map(|parts| parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" / "))
                    .unwrap_or_else(|| format!("{} ({})", row["name"].as_str().unwrap_or_default(), tl!("unknown path")));
                if button(app, ui, &format!("binImport.bin.{id}"), &format!("{path} (#{id})")) {
                    return Some(Selection::Preview { revision: rev.clone(), bin: id });
                }
            }
        }
        Selection::Preview { revision, bin } => {
            let selected = page["bins"].as_array().into_iter().flatten().find(|record| record["id"].as_u64() == Some(*bin));
            ui.heading(selected.and_then(|v| v["name"].as_str()).unwrap_or_default());
            ui.label(format!("{} {}", tl!("Revision"), revision.get(..8).unwrap_or(revision)));
            for (key, title) in [("bins", tl!("Bins")), ("filed_items", tl!("Filed Items")), ("referenced_items", tl!("Referenced Items"))] {
                if let Some(records) = page[key].as_array() {
                    let title = format!("{title} ({})", records.len());
                    ui.strong(&title);
                    for record in records {
                        ui.label(record["name"].as_str().unwrap_or_default());
                    }
                }
            }
        }
    }
    None
}

pub(in crate::panels) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut b) = app.ui.project_panel.external_import.take() else { return };
    if app.session.document_epoch != b.epoch || !app.session.is_modular_document() {
        return;
    }
    if let Some(page) = b.pending.as_ref().and_then(Pending::poll) {
        b.page = page;
        b.pending = None;
    }
    let mut next = None;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("saved-bin-import")).show(ctx, |ui| {
        ui.set_width(440.0_f32.min(ctx.content_rect().width() * 0.9));
        ui.heading(tl!("Import Saved Bin"));
        let folder = std::path::Path::new(&b.folder).file_name().and_then(|n| n.to_str()).unwrap_or(&b.folder);
        ui.label(format!("{} {}", tl!("Source folder:"), folder)).on_hover_text(&b.folder);
        let name = app.session.project.root.find_bin(BinId(b.destination)).map_or(tl!("Unavailable"), |bin| bin.name.as_str());
        ui.label(format!("{} {} (#{})", tl!("Destination:"), name, b.destination));
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt(("import-contents", std::mem::discriminant(&b.selection)))
            .max_height(300.0_f32.min(ctx.content_rect().height() * 0.48))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                next = browse_page(&b, app, ui);
            });
        ui.separator();
        ui.horizontal(|ui| {
            close = button(app, ui, "binImport.cancel", tl!("Cancel"));
            if b.selection != Selection::Saves && button(app, ui, "binImport.back", tl!("Back")) {
                next = Some(match &b.selection {
                    Selection::Preview { revision, .. } => Selection::Bins(revision.clone()),
                    _ => Selection::Saves,
                });
            }
            if let Ok(page) = &b.page
                && let Some(head) = page["head"].as_str()
            {
                let r = ui
                    .add_enabled(app.session.can_import_into_bin(b.destination), egui::Button::new(tl!("Import")))
                    .on_disabled_hover_text(tl!("Open the destination bin for writing first"));
                app.auto.add("binImport.apply", r.rect, tl!("Import"));
                if r.clicked() {
                    let mut args = b.params();
                    args["head"] = head.into();
                    args["epoch"] = b.epoch.into();
                    args["bin"] = b.destination.into();
                    match app.session.execute("project.importExternalBin", args) {
                        Ok(_) => {
                            app.ui.status = tl!("Imported saved bin").to_owned();
                            close = true;
                        }
                        Err(e) => b.page = Err(e.to_string()),
                    }
                }
            }
        });
    });
    if !close && !crate::framework::take_window_cancel(ctx) && !modal.should_close() {
        if let Some(selection) = next {
            b.read(app, ctx, selection);
        }
        app.ui.project_panel.external_import = Some(b);
    }
}
