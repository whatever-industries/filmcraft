//! A pinned historical view, separate from the live bin selection and checkout.
use crate::FilmcraftApp;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub struct Browser {
    epoch: u64,
    initial_bin: Option<u64>,
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
    let mut visible = true;
    let mut next = None;
    let mut close = false;
    egui::Window::new(if b.initial_bin.is_some() { tl!("Bin History") } else { tl!("Project History") })
        .id(egui::Id::unique("saved-bin-browser"))
        .open(&mut visible)
        .show(ctx, |ui| {
            if button(app, ui, "binHistory.close", tl!("Close")) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            if let Some(revision) = &b.revision {
                ui.horizontal(|ui| {
                    if ui.button(tl!("Revisions")).clicked() {
                        next = Some(b.initial_bin.map_or(json!({}), |id| json!({"bin":id})));
                    }
                    if b.page["rows"].is_array() && ui.button(tl!("Browse Bins")).clicked() {
                        next = Some(json!({"revision":revision}));
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
                        let label = format!("{} {}", tl!("Revision"), rev[0]);
                        if button(app, ui, &format!("binHistory.revision.{}", rev[0]), &label) {
                            let mut p = json!({"revision":rev[1]});
                            if let Some(id) = b.initial_bin {
                                p["bin"] = id.into();
                            }
                            next = Some(p);
                        }
                    }
                } else if let Some(bins) = b.page["bins"].as_array() {
                    for row in bins {
                        let path = row["path"]
                            .as_array()
                            .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" / "))
                            .unwrap_or_else(|| format!("{} ({})", row["name"].as_str().unwrap_or_default(), tl!("Unknown path")));
                        if button(app, ui, &format!("binHistory.bin.{}", row["id"]), &format!("{path}  #{}", row["id"])) {
                            next = Some(json!({"revision":b.revision,"bin":row["id"]}));
                        }
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
                                for cell in row["cells"].as_array().into_iter().flatten().skip(1) {
                                    ui.label(cell.as_str().unwrap_or_default());
                                }
                            }
                            ui.end_row();
                        }
                    });
                }
            });
        });
    if let Some(params) = next
        && let Some(page) = query(app, params.clone())
    {
        b.revision = params["revision"].as_str().map(str::to_owned);
        b.page = page;
        b.selected.clear();
    }
    if visible && !close {
        app.ui.project_panel.history_browser = Some(b);
    }
}
