//! Navigate from a saved item to its consuming sequences without opening a live cut.
use super::{FilmcraftApp, button, worker::Pending};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
struct Sequence {
    id: u64,
    name: String,
    direct: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct SavedBin {
    id: u64,
    label: String,
    known_path: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Report {
    sequences: Vec<Sequence>,
    bins: Vec<SavedBin>,
    // A filing may be shared by many results. Store each display path once.
    locations: BTreeMap<u64, Vec<usize>>,
    complete: bool,
    bin_errors: BTreeMap<u64, String>,
    item_errors: BTreeMap<u64, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Uses {
    name: String,
    pending: Option<Pending<Report>>,
    report: Option<Report>,
    pub error: Option<String>,
}

impl Uses {
    pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context, revision: &str, item: u64, name: &str) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let result = app.session.prepare_saved_item_uses(revision, item).map_err(|e| e.to_string()).and_then(|read| {
            app.bin_history_worker.submit(ctx, move || {
                let usage = read().map_err(|e| e.to_string())?;
                let mut bins = Vec::new();
                let mut locations = BTreeMap::<_, Vec<_>>::new();
                for filing in usage.filings {
                    let index = bins.len();
                    let bin = filing.bin;
                    bins.push(SavedBin { id: bin.id, known_path: bin.path.is_some(), label: bin.path.map_or(bin.name, |p| p.join(" / ")) });
                    for id in filing.items {
                        locations.entry(id).or_default().push(index);
                    }
                }
                Ok(Report {
                    sequences: usage.sequences.into_iter().map(|s| Sequence { id: s.id, name: s.name, direct: s.direct }).collect(),
                    bins,
                    locations,
                    complete: usage.complete,
                    bin_errors: usage.bin_errors,
                    item_errors: usage.item_errors,
                })
            })
        });
        #[cfg(target_arch = "wasm32")]
        let result = {
            let _ = (app, ctx, revision, item);
            Err(tl!("Find Uses requires a local project folder").to_owned())
        };
        let (pending, error) = match result {
            Ok(pending) => (Some(pending), None),
            Err(e) => (None, Some(e)),
        };
        Self { name: name.to_owned(), pending, report: None, error }
    }

    pub fn show(&mut self, app: &mut FilmcraftApp, ui: &mut egui::Ui, revision: &str) -> Option<(u64, u64)> {
        ui.heading(&self.name);
        ui.label(format!("{} {}", tl!("Revision"), revision.get(..8).unwrap_or(revision)));
        if let Some(reply) = self.pending.as_ref().and_then(Pending::poll) {
            self.pending = None;
            match reply {
                Ok(report) => self.report = Some(report),
                Err(error) => self.error = Some(error),
            }
        }
        if self.pending.is_some() {
            let response = ui.label(tl!("Searching saved sequences…"));
            if ui.ctx().has_accesskit_node(response.id) {
                ui.ctx().accesskit_node_builder(response.id, |node| node.set_live(egui::accesskit::Live::Polite));
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        if let Some(message) = &self.error {
            ui.label(message);
        }
        let report = self.report.as_ref()?;
        let mut jump = None;
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            if !report.complete {
                ui.strong(tl!("Search incomplete"));
                ui.collapsing(tl!("Unreadable records"), |ui| {
                    for (kind, errors) in [(tl!("Bin"), &report.bin_errors), (tl!("Item"), &report.item_errors)] {
                        for (id, error) in errors {
                            ui.label(format!("{kind} #{id} · {error}"));
                        }
                    }
                });
            }
            if report.sequences.is_empty() {
                let label = if report.complete { tl!("No sequences use this item in this save") } else { tl!("No verified matches") };
                let response = ui.label(label);
                app.auto.add("binHistory.noUses", response.rect, label);
            }
            for Sequence { id, name, direct } in &report.sequences {
                ui.separator();
                ui.strong(format!("{name} #{id} · {}", if *direct { tl!("Direct use") } else { tl!("Indirect use") }));
                if let Some(indices) = report.locations.get(id) {
                    for &index in indices {
                        let SavedBin { id: bin, label: path, known_path } = &report.bins[index];
                        let mut label = format!("{path} #{bin}");
                        if !known_path {
                            label.push_str(&format!(" · {}", tl!("Unknown path")));
                        }
                        ui.push_id((id, bin), |ui| {
                            if button(app, ui, &format!("binHistory.use.{id}.{bin}"), &label) {
                                jump = Some((*bin, *id));
                            }
                        });
                    }
                } else {
                    ui.label(tl!("No known bin filing"));
                }
            }
        });
        jump
    }
}
