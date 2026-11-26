//! A pinned historical view, separate from the live bin selection and checkout.
mod versions;
mod worker;
use crate::FilmcraftApp;
use serde_json::{Value, json};
use std::collections::BTreeSet;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use worker::Worker;

#[derive(Clone, Debug, PartialEq)]
pub struct Clipboard {
    epoch: u64,
    revision: String,
    items: Vec<u64>,
}

pub(super) fn paste_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, bin: u64) -> bool {
    let Some(clip) = app.ui.project_panel.history_clipboard.as_ref().filter(|clip| clip.epoch == app.session.document_epoch) else {
        return false;
    };
    let enabled = !app.session.editing_bins() || app.session.bin_access(bin) == Some(true);
    let response = ui.add_enabled(enabled, egui::Button::new(tl!("Paste from Bin History")));
    app.auto.add("binHistory.paste", response.rect, tl!("Paste from Bin History"));
    if response.clicked() {
        let p = json!({"epoch":clip.epoch,"revision":clip.revision,"items":clip.items,"bin":bin});
        match app.session.execute("project.pasteFromHistory", p) {
            Ok(_) => app.ui.status = tl!("Pasted from Bin History").to_owned(),
            Err(e) => app.ui.status = e.to_string(),
        }
        return true;
    }
    false
}

#[derive(Clone, Debug, PartialEq)]
pub struct Browser {
    epoch: u64,
    initial_bin: Option<u64>,
    live_revision: u64,
    comparison: Option<versions::Comparison>,
    revision: Option<String>,
    page: Value,
    selected: BTreeSet<u64>,
    show_references: bool,
}

pub(super) fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str) -> bool {
    let response = ui.button(label);
    app.auto.add(id, response.rect, label);
    response.clicked()
}

fn query(app: &mut FilmcraftApp, p: Value) -> Option<Value> {
    match app.session.execute("project.binHistory", p) {
        Ok(value) => Some(value),
        Err(e) => {
            app.ui.status = e.to_string();
            None
        }
    }
}

pub fn open(app: &mut FilmcraftApp, bin: Option<u64>) {
    let p = bin.map_or(json!({}), |id| json!({"bin":id}));
    if let Some(page) = query(app, p) {
        app.ui.project_panel.history_browser = Some(Browser {
            epoch: app.session.document_epoch,
            initial_bin: bin,
            live_revision: app.session.revision,
            comparison: None,
            revision: None,
            page,
            selected: BTreeSet::new(),
            show_references: app.session.prefs.project_panel.show_reference_clips,
        });
    }
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut b) = app.ui.project_panel.history_browser.take() else { return };
    if b.epoch != app.session.document_epoch || !app.session.is_modular_document() {
        return;
    }
    if b.comparison.is_none()
        && b.live_revision != app.session.revision
        && let (Some(hash), Some(bin)) = (&b.revision, b.page["bin"].as_u64())
        && let Some(page) = query(app, json!({"revision":hash,"bin":bin}))
    {
        b.page = page;
        b.live_revision = app.session.revision;
    }
    let mut visible = true;
    let mut next = None;
    let mut close = false;
    let response = egui::Window::new(if b.comparison.is_some() {
        tl!("Compare Versions")
    } else if b.initial_bin.is_some() {
        tl!("Bin History")
    } else {
        tl!("Project History")
    })
    .id(egui::Id::unique("saved-bin-browser"))
    .open(&mut visible)
    .show(ctx, |ui| {
        if button(app, ui, "binHistory.close", tl!("Close")) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if let Some(comparison) = &mut b.comparison {
            if button(app, ui, "binHistory.back", tl!("Back to Bin History")) {
                b.comparison = None;
            } else if let Some((revision, item)) = comparison.show(app, ui) {
                app.ui.project_panel.history_clipboard = Some(Clipboard { epoch: b.epoch, revision, items: vec![item] });
                app.ui.status = tl!("Copied from Bin History").to_owned();
                close = true;
            }
            return;
        }
        if let Some(revision) = &b.revision {
            ui.horizontal(|ui| {
                if ui.button(tl!("Revisions")).clicked() {
                    next = Some(b.initial_bin.map_or(json!({}), |id| json!({"bin":id})));
                }
                if b.page["rows"].is_array() && button(app, ui, "binHistory.bins", tl!("Browse Bins")) {
                    next = Some(json!({"revision":revision}));
                }
                if let Some(bin) = b.page["bin"].as_u64()
                    && button(app, ui, "binHistory.compare", tl!("Compare Versions"))
                {
                    b.comparison = Some(versions::Comparison::open(app, ctx, bin, revision));
                }
                ui.label(&revision[..8]);
            });
        }
        if let Some(name) = b.page["name"].as_str() {
            ui.heading(name);
            let toggle = ui.checkbox(&mut b.show_references, tl!("Show Reference Clips"));
            app.auto.add("binHistory.references", toggle.rect, tl!("Show Reference Clips"));
        }
        egui::ScrollArea::both().max_height(360.0).show(ui, |ui| {
            if let Some(revs) = b.page["revisions"].as_array() {
                for rev in revs {
                    ui.horizontal(|ui| {
                        let label = format!("{} {}", tl!("Revision"), rev[0]);
                        if button(app, ui, &format!("binHistory.revision.{}", rev[0]), &label) {
                            let mut p = json!({"revision":rev[1]});
                            if let Some(id) = b.initial_bin {
                                p["bin"] = id.into();
                            }
                            next = Some(p);
                        }
                        if let (Some(bin), Some(hash)) = (b.initial_bin, rev[1].as_str())
                            && button(app, ui, &format!("binHistory.compareRevision.{}", rev[0]), tl!("Compare Versions"))
                        {
                            b.comparison = Some(versions::Comparison::open(app, ctx, bin, hash));
                        }
                    });
                }
            } else if let Some(bins) = b.page["bins"].as_array() {
                for row in bins {
                    let path = row["path"]
                        .as_array()
                        .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" / "))
                        .unwrap_or_else(|| format!("{} ({})", row["name"].as_str().unwrap_or_default(), tl!("Unknown path")));
                    ui.horizontal(|ui| {
                        if button(app, ui, &format!("binHistory.bin.{}", row["id"]), &format!("{path}  #{}", row["id"])) {
                            next = Some(json!({"revision":b.revision,"bin":row["id"]}));
                        }
                        if let (Some(bin), Some(hash)) = (row["id"].as_u64(), &b.revision)
                            && button(app, ui, &format!("binHistory.compareBin.{bin}"), tl!("Compare Versions"))
                        {
                            b.comparison = Some(versions::Comparison::open(app, ctx, bin, hash));
                        }
                    });
                }
                if let Some(errors) = b.page["errors"].as_object() {
                    for msg in errors.values().filter_map(Value::as_str) {
                        ui.label(msg);
                    }
                }
            } else if let Some(rows) = b.page["rows"].as_array() {
                egui::Grid::new("saved-bin-rows").striped(true).show(ui, |ui| {
                    for col in b.page["columns"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        ui.strong(col);
                    }
                    ui.end_row();
                    for row in rows {
                        let name = row["name"].as_str().unwrap_or_default();
                        if let Some(id) = row["bin"].as_u64() {
                            if ui.button(name).clicked() {
                                next = Some(json!({"revision":b.revision,"bin":id}));
                            }
                        } else if let Some(id) = row["item"].as_u64() {
                            let reference = row["reference"] == true;
                            if reference && !b.show_references {
                                continue;
                            }
                            let label = if reference { format!("{name} · {}", tl!("Reference")) } else { name.to_owned() };
                            let label = if row["savedMetadata"] == true { format!("{label} · {}", tl!("Saved metadata")) } else { label };
                            let mut selected = b.selected.contains(&id);
                            let r = ui.checkbox(&mut selected, &label);
                            app.auto.add(&format!("binHistory.item.{id}"), r.rect, &label);
                            if r.changed() {
                                if selected {
                                    b.selected.insert(id);
                                } else {
                                    b.selected.remove(&id);
                                }
                            }
                            for (index, cell) in row["cells"].as_array().into_iter().flatten().enumerate().skip(1) {
                                let text = cell.as_str().unwrap_or_default();
                                let r = ui.label(text);
                                app.auto.add(&format!("binHistory.cell.{id}.{index}"), r.rect, text);
                            }
                        }
                        ui.end_row();
                    }
                });
            }
        });
        if let Some(rows) = b.page["rows"].as_array() {
            let selected: Vec<_> = rows
                .iter()
                .filter(|row| b.show_references || row["reference"] != true)
                .filter_map(|row| row["item"].as_u64())
                .filter(|id| b.selected.contains(id))
                .collect();
            let r = ui.add_enabled(!selected.is_empty(), egui::Button::new(tl!("Copy Selected Items")));
            app.auto.add("binHistory.copy", r.rect, tl!("Copy Selected Items"));
            if r.clicked()
                && let Some(revision) = &b.revision
            {
                app.ui.project_panel.history_clipboard = Some(Clipboard { epoch: b.epoch, revision: revision.clone(), items: selected });
                app.ui.status = tl!("Copied from Bin History").to_owned();
                close = true;
            }
        }
    });
    if let Some(params) = next
        && let Some(page) = query(app, params.clone())
    {
        b.revision = params["revision"].as_str().map(str::to_owned);
        b.page = page;
        b.live_revision = app.session.revision;
        b.selected.clear();
    }
    if visible && !close && !response.is_some_and(|r| r.response.should_close()) {
        app.ui.project_panel.history_browser = Some(b);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
