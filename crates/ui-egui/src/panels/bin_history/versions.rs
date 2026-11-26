//! Compare pinned saves and copy through the ordinary Bin History clipboard.
use super::{FilmcraftApp, button};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Comparison {
    bin: u64,
    revisions: Vec<(u64, String)>,
    from: Option<String>,
    to: String,
    sequence: Option<u64>,
    bin_names: [String; 2],
    report: Value,
    error: Option<String>,
}

fn value(v: Option<&Value>) -> String {
    match v {
        None => tl!("Absent").to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn fields(app: &mut FilmcraftApp, ui: &mut egui::Ui, fields: &Value, prefix: &str) {
    for field in fields.as_array().into_iter().flatten() {
        if let Some(path) = field["path"].as_str() {
            let mut name = path.trim_start_matches('/').split('/').map(|part| part.replace("~1", "/").replace("~0", "~")).collect::<Vec<_>>().join(" › ");
            if let Some(key) = path.strip_prefix("/metadata/")
                && filmcraft_engine::project_panel::is_freeform_key(key)
            {
                name = format!("{} › {key}", tl!("Organization"));
            }
            let text = format!("{name}   {} → {}", value(field.get("before")), value(field.get("after")));
            let row = ui.label(&text);
            app.auto.add(&format!("{prefix}{path}"), row.rect, &text);
        } else if field["more"] == true {
            ui.label(tl!("More fields changed"));
        }
    }
}

fn kind(s: &str) -> &str {
    match s {
        "added" => tl!("Added"),
        "removed" => tl!("Removed"),
        "moved" => tl!("Moved"),
        "renamed" => tl!("Renamed"),
        "reordered" => tl!("Reordered"),
        "other" => tl!("Fields changed"),
        "sequenceChanged" => tl!("Sequence edited"),
        "filingChanged" => tl!("Filing changed"),
        "associatedMetadata" => tl!("Associated records changed"),
        "trackMoved" => tl!("Changed track"),
        "timelineMoved" => tl!("Moved in timeline"),
        "trimmed" => tl!("Duration changed"),
        "sourceReplaced" => tl!("Source replaced"),
        "sourceInChanged" => tl!("Source In changed"),
        _ => s,
    }
}

impl Comparison {
    fn copy_buttons(&self, app: &mut FilmcraftApp, ui: &mut egui::Ui, id: u64, first: bool, second: bool) -> Option<(String, u64)> {
        for (side, present, revision) in [("before", first, self.from.as_deref()), ("after", second, Some(self.to.as_str()))] {
            let Some(hash) = revision.filter(|_| present) else { continue };
            let label = match (side, self.sequence.is_some()) {
                ("before", true) => tl!("Copy sequence from first save"),
                ("after", true) => tl!("Copy sequence from second save"),
                ("before", false) => tl!("Copy from first save"),
                _ => tl!("Copy from second save"),
            };
            if button(app, ui, &format!("binHistory.copy.{side}.{id}"), label) {
                return Some((hash.to_owned(), id));
            }
        }
        None
    }

    pub fn open(app: &mut FilmcraftApp, bin: u64, to: &str) -> Option<Self> {
        let data = match app.session.execute("project.compareBinVersions", json!({"bin":bin,"to":to})) {
            Ok(data) => data,
            Err(e) => {
                app.ui.status = e.to_string();
                return None;
            }
        };
        Some(Self {
            bin,
            revisions: serde_json::from_value(data["revisions"].clone()).ok()?,
            from: data["from"].as_str().map(str::to_owned),
            to: to.into(),
            sequence: None,
            bin_names: Self::names(&data["report"]),
            report: data["report"].clone(),
            error: data["lookupError"].as_str().map(str::to_owned),
        })
    }

    fn names(report: &Value) -> [String; 2] {
        ["before_name", "after_name"].map(|key| report[key].as_str().unwrap_or_default().to_owned())
    }

    fn load(&mut self, app: &mut FilmcraftApp) {
        let Some(from) = &self.from else { return };
        let mut params = json!({"bin":self.bin,"from":from,"to":self.to});
        if let Some(seq) = self.sequence {
            params["sequence"] = seq.into();
        }
        match app.session.execute("project.compareBinVersions", params) {
            Ok(data) => {
                self.report = data["report"].clone();
                if self.sequence.is_none() {
                    self.bin_names = Self::names(&self.report);
                }
                self.error = None;
            }
            Err(e) => {
                self.report = Value::Null;
                if self.sequence.is_none() {
                    self.bin_names = Default::default();
                }
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn show(&mut self, app: &mut FilmcraftApp, ui: &mut egui::Ui) -> Option<(String, u64)> {
        let mut reload = false;
        ui.horizontal(|ui| {
            for before in [true, false] {
                let selected = if before { self.from.as_deref() } else { Some(self.to.as_str()) };
                let title = if before { tl!("From") } else { tl!("To") };
                let number = self
                    .revisions
                    .iter()
                    .find(|(_, h)| Some(h.as_str()) == selected)
                    .map(|(n, _)| n.to_string())
                    .unwrap_or_else(|| tl!("Choose save").to_owned());
                let name = &self.bin_names[usize::from(!before)];
                ui.menu_button(format!("{title} · {number} {name}"), |ui| {
                    egui::ScrollArea::vertical().max_height(230.0).show(ui, |ui| {
                        for (n, hash) in &self.revisions {
                            let label = format!("{} {n}", tl!("Revision"));
                            if button(app, ui, &format!("binHistory.{}.{n}", if before { "from" } else { "to" }), &label) {
                                if before {
                                    self.from = Some(hash.clone());
                                } else {
                                    self.to = hash.clone();
                                }
                                self.sequence = None;
                                reload = true;
                                ui.close();
                            }
                        }
                    });
                });
            }
        });
        if self.sequence.is_some() && button(app, ui, "binHistory.summary", tl!("Back to bin changes")) {
            self.sequence = None;
            reload = true;
        }
        if reload {
            self.load(app);
        }
        if let Some(msg) = &self.error {
            ui.label(msg);
        }
        if self.from.is_none() && self.error.is_none() {
            ui.label(tl!("No previous version of this bin"));
        }
        let rows = self.report["rows"].as_array()?;
        if rows.is_empty() {
            ui.label(tl!("No differences"));
        }
        if self.report["complete"] == false {
            ui.label(tl!("Comparison is incomplete"));
        }
        let mut drill = None;
        let mut copy = self.sequence.and_then(|id| {
            ui.horizontal(|ui| self.copy_buttons(app, ui, id, self.report["before_name"].is_string(), self.report["after_name"].is_string())).inner
        });
        egui::ScrollArea::both().max_height(390.0).show(ui, |ui| {
            for (i, r) in rows.iter().enumerate() {
                if let Some(e) = r["error"].as_str() {
                    let revision = r["revision"].as_str().unwrap_or_default();
                    ui.label(format!("{} · {} · {e}", revision.get(..8).unwrap_or(revision), value(r.get("record"))));
                    continue;
                }
                ui.separator();
                let id = r["record"]["Item"].as_u64();
                let name = r["afterName"]
                    .as_str()
                    .or(r["beforeName"].as_str())
                    .or(self.report["after_name"].as_str())
                    .or(self.report["before_name"].as_str())
                    .unwrap_or_default();
                let changes = r["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .chain(r["change"].as_str())
                    .map(kind)
                    .collect::<Vec<_>>()
                    .join(", ");
                if r["scope"] == "dependency" {
                    ui.strong(format!("{name} · {} · {changes}", tl!("Reference")));
                } else {
                    ui.strong(format!("{name} · {changes}"));
                }
                if r["change"] == "reordered" {
                    ui.label(format!("{} → {}", value(r.get("before")), value(r.get("after"))));
                }
                if r["changes"].as_array().is_some_and(|changes| changes.iter().any(|c| c == "moved")) {
                    ui.label(format!("{}   {} → {}", tl!("Bin"), value(r.get("beforeParent")), value(r.get("afterParent"))));
                }
                if let Some(clip) = r["clip"].as_u64() {
                    ui.label(format!("{} #{clip}", tl!("Timeline clip")));
                    for (key, label) in [
                        ("track", tl!("Track")),
                        ("startTimecode", tl!("Time")),
                        ("durationFrames", tl!("Duration (frames)")),
                        ("source", tl!("Source")),
                        ("sourceInAtSequenceRate", tl!("Source In")),
                    ] {
                        ui.label(format!("{label}   {} → {}", value(r["before"].get(key)), value(r["after"].get(key))));
                    }
                }
                ui.push_id(i, |ui| {
                    if r["fields"].as_array().is_some_and(|v| !v.is_empty()) || r["associatedFields"].as_array().is_some_and(|v| !v.is_empty()) {
                        let header = ui.collapsing(tl!("Changed fields"), |ui| {
                            fields(app, ui, &r["fields"], &format!("binHistory.field.{i}"));
                            fields(app, ui, &r["associatedFields"], &format!("binHistory.associated.{i}"));
                        });
                        app.auto.add(&format!("binHistory.fields.{i}"), header.header_response.rect, tl!("Changed fields"));
                    }
                    if let Some(id) = id
                        && self.sequence.is_none()
                    {
                        ui.horizontal(|ui| {
                            if r["itemType"] == "sequence" && button(app, ui, &format!("binHistory.inspect.{id}"), tl!("Inspect sequence")) {
                                drill = Some(id);
                            }
                            if let Some(selected) = self.copy_buttons(app, ui, id, r["beforeName"].is_string(), r["afterName"].is_string()) {
                                copy = Some(selected);
                            }
                        });
                    }
                });
            }
        });
        if drill.is_some() {
            self.sequence = drill;
            self.load(app);
        }
        copy
    }
}
