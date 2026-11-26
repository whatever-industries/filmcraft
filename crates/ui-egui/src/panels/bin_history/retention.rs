use super::{FilmcraftApp, button, worker::Pending};
use std::num::NonZeroUsize;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Retention {
    keep: usize,
    pending: Option<Pending<Summary>>,
    result: Option<Result<Summary, String>>,
}

#[derive(Clone, Debug, PartialEq)]
struct Summary {
    head: String,
    retained: u64,
    older: u64,
    loose: u64,
    reclaimable: u64,
    readers: usize,
    drafts: usize,
}

impl Retention {
    pub fn open(app: &mut FilmcraftApp, ctx: &egui::Context) -> Self {
        let mut view = Self { keep: 1, pending: None, result: None };
        view.run(app, ctx);
        view
    }

    fn run(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context) {
        let read = app.session.prepare_retention_preview(NonZeroUsize::new(self.keep).unwrap_or(NonZeroUsize::MIN));
        match read.map_err(|e| e.to_string()).and_then(|read| {
            app.bin_history_worker.submit(ctx, move || {
                let r = read().map_err(|e| e.to_string())?;
                Ok(Summary {
                    readers: r.saves.iter().filter(|s| s.holds.supplied_reader).count(),
                    drafts: r.saves.iter().filter(|s| s.holds.supplied_draft).count(),
                    head: r.head,
                    retained: r.retained_policy_bytes,
                    older: r.older_only_payload_bytes,
                    loose: r.unreferenced_digest_bytes,
                    reclaimable: r.confirmed_reclaimable_bytes,
                })
            })
        }) {
            Ok(job) => {
                self.pending = Some(job);
                self.result = None;
            }
            Err(e) => {
                self.pending = None;
                self.result = Some(Err(e));
            }
        }
    }

    pub fn show(&mut self, app: &mut FilmcraftApp, ui: &mut egui::Ui, saves: usize) {
        let count = ui.add(egui::Slider::new(&mut self.keep, 1..=saves.max(1)).text(tl!("Recent saves to keep")));
        app.auto.add("retention.keepLatest", count.rect, tl!("Recent saves to keep"));
        if count.changed() {
            self.pending = None;
            self.result = None;
        }
        if button(app, ui, "retention.refresh", tl!("Run preview")) {
            self.run(app, ui.ctx());
        }
        if let Some(reply) = self.pending.as_ref().and_then(Pending::poll) {
            self.result = Some(reply);
            self.pending = None;
        }
        if self.pending.is_some() {
            ui.label(tl!("Checking saved history…"));
        }
        match &self.result {
            Some(Ok(r)) => {
                ui.label(format!("{} {}", tl!("Published revision"), r.head.get(..8).unwrap_or(&r.head)));
                egui::Grid::new("retention-summary").show(ui, |ui| {
                    for (label, value) in [
                        (tl!("Retained by this policy"), r.retained),
                        (tl!("Used only by older saves"), r.older),
                        (tl!("Unreferenced files"), r.loose),
                        (tl!("Confirmed reclaimable"), r.reclaimable),
                    ] {
                        ui.label(label);
                        ui.label(if value < 1000 { format!("{value} B") } else { filmcraft_engine::export::format_bytes(value) })
                            .on_hover_text(format!("{value} B"));
                        ui.end_row();
                    }
                });
                ui.label(format!("{} · {}     {} · {}", tl!("Editor and Undo/Redo revisions"), r.readers, tl!("Local checkpoint revisions"), r.drafts));
                ui.label(tl!("Other readers and profiles may hold additional revisions."));
            }
            Some(Err(e)) => {
                ui.label(e);
            }
            None => {}
        }
    }
}
