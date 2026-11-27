//! A modular bin has one view for the lifetime of its editing session.
use super::{BinTab, FilmcraftApp, Inst};
use filmcraft_project::{BinEntry, BinId, ItemId};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct Runtime {
    epoch: Option<u64>,
    revision: u64,
    published: String,
    dirty: BTreeSet<BinId>,
    prompt: Option<(u64, BinId)>,
    focused: Option<u64>,
    pub(super) bring_forward: Option<u64>,
    focus_answer: bool,
}

impl Runtime {
    pub(crate) fn prompt_open(&self) -> bool {
        self.prompt.is_some()
    }
}

pub(super) fn modular(app: &FilmcraftApp) -> bool {
    app.session.folder_revision().is_some()
}

pub(crate) fn sync(app: &mut FilmcraftApp) {
    let epoch = app.session.document_epoch;
    let changed = app.bin_views.epoch != Some(epoch);
    if changed {
        if app.bin_views.epoch.is_some() && !app.bin_views.published.is_empty() {
            app.ui.project_panel.tabs.clear();
            app.ui.project_panel.bin = None;
            app.ui.project_panel.active_tab = None;
        }
        app.bin_views = Runtime { epoch: Some(epoch), ..Default::default() };
    }
    let Some(revision) = app.session.folder_revision() else { return };
    if changed || app.bin_views.revision != app.session.revision || app.bin_views.published != revision {
        app.bin_views.published = revision.to_owned();
        app.bin_views.revision = app.session.revision;
        app.bin_views.dirty = app.session.dirty_folder_bins();
    }
    if changed {
        normalize(app);
    }
}

/// Import older layouts with in-place navigation and duplicate window/tab entries.
/// The root project browser is the permanent home tab.
pub(crate) fn normalize(app: &mut FilmcraftApp) {
    if !modular(app) {
        return;
    }
    let pp = &mut app.ui.project_panel;
    let root = &app.session.project.root;
    let active = pp.active_tab.and_then(|k| pp.tabs.get(k)).map(|t| t.nav.unwrap_or(t.bin)).or(pp.bin);
    let mut restored = Vec::<BinTab>::new();
    let mut seen = BTreeSet::new();
    let mut add = |t: BinTab| {
        if t.bin != root.id.0 && root.find_bin(BinId(t.bin)).is_some() && seen.insert(t.bin) {
            restored.push(t);
        }
    };
    for mut t in std::mem::take(&mut pp.tabs) {
        let nav = t.nav.take();
        if let Some(bin) = nav.filter(|b| *b != t.bin) {
            add(BinTab { floating: false, ..t.clone() });
            t.bin = bin;
        }
        add(t);
    }
    if let Some(bin) = pp.bin.take() {
        add(BinTab { bin, ..Default::default() });
    }
    pp.tabs = restored;
    pp.active_tab = active.and_then(|bin| pp.tabs.iter().position(|t| t.bin == bin && !t.floating));
}

pub(super) fn open(app: &mut FilmcraftApp, inst: Inst, bin: u64, how: &str, explicit: bool) -> Result<Value, String> {
    if !matches!(how, "openInPlace" | "openNewTab" | "openNewWindow") {
        return Err(format!("unknown bin open mode {how}"));
    }
    let root = app.session.project.root.id.0;
    if bin == root {
        app.ui.project_panel.active_tab = None;
        app.bin_views.focused = None;
        app.bin_views.bring_forward = Some(root);
        app.show_panel(crate::PanelKind::Project);
        return Ok(json!({"opened":"inPlace","bin":bin}));
    }
    let pp = &mut app.ui.project_panel;
    let previous = match inst {
        Inst::Tab(k) => pp.tabs.get(k).cloned(),
        Inst::Main => None,
    };
    let floating = how == "openNewWindow" || how == "openInPlace" && previous.as_ref().is_some_and(|t| t.floating);
    let existing = pp.tabs.iter().position(|t| t.bin == bin);
    let k = if let Some(k) = existing {
        if explicit && how != "openInPlace" {
            pp.tabs[k].floating = floating;
        }
        k
    } else {
        let prefs = &app.session.prefs.project_panel.view;
        if how == "openInPlace"
            && let Inst::Tab(k) = inst
            && let Some(t) = pp.tabs.get_mut(k)
        {
            t.floating = false;
        }
        pp.tabs.push(BinTab {
            bin,
            floating,
            view: previous.as_ref().map_or(prefs.mode, |t| t.view),
            icon_size: previous.map_or(prefs.icon_size, |t| t.icon_size),
            nav: None,
        });
        pp.tabs.len() - 1
    };
    let floating = pp.tabs[k].floating;
    if !floating {
        pp.active_tab = Some(k);
    } else if pp.active_tab == Some(k) {
        pp.active_tab = None;
    }
    app.bin_views.focused = Some(bin);
    app.bin_views.bring_forward = Some(bin);
    if !floating {
        app.show_panel(crate::PanelKind::Project);
    }
    Ok(json!({"opened":if floating{"window"}else{"tab"},"bin":bin,"tab":k}))
}

pub(crate) fn owns_button_focus(app: &FilmcraftApp, ctx: &egui::Context) -> bool {
    if !modular(app) {
        return false;
    }
    let Some(id) = ctx.memory(|m| m.focused()) else {
        return false;
    };
    id == egui::Id::unique("project.tab.project")
        || id == egui::Id::unique(("project", "up"))
        || app.ui.project_panel.tabs.iter().enumerate().any(|(k, _)| {
            id == egui::Id::unique(format!("project.tab.{k}"))
                || id == egui::Id::unique(("ptab-close", k))
                || id == egui::Id::unique((format!("projectBin.{k}"), "up"))
        })
}
pub(super) fn focused_index(app: &FilmcraftApp) -> Option<usize> {
    if !modular(app) {
        return app.ui.project_panel.active_tab;
    }
    app.bin_views.focused.and_then(|id| app.ui.project_panel.tabs.iter().position(|t| t.bin == id && t.floating)).or(app.ui.project_panel.active_tab)
}
pub(super) fn focus(app: &mut FilmcraftApp, bin: Option<u64>) {
    app.bin_views.focused = bin;
}
pub(super) fn label(app: &FilmcraftApp, bin: u64, name: &str) -> String {
    if app.bin_views.dirty.contains(&BinId(bin)) { tlf!("{name} (Unsaved)", name) } else { name.to_owned() }
}

pub(super) fn request_close(app: &mut FilmcraftApp, k: usize) -> Result<Value, String> {
    sync(app);
    let bin = BinId(app.ui.project_panel.tabs.get(k).ok_or("no such bin tab")?.bin);
    if !modular(app) {
        super::remove_tab(app, k);
        return Ok(Value::Null);
    }
    if app.bin_views.prompt.is_some() {
        return Err(tl!("Answer the open bin’s save prompt first.").to_string());
    }
    if app.bin_views.dirty.contains(&bin) {
        app.bin_views.prompt = Some((app.session.document_epoch, bin));
        app.bin_views.focus_answer = true;
        return Ok(json!({"bin":bin,"needsDecision":true}));
    }
    finish(app, bin, app.session.bin_needs_cleanup(bin).then_some("project.saveBin"))
}

fn finish(app: &mut FilmcraftApp, bin: BinId, decision: Option<&str>) -> Result<Value, String> {
    let k = app.ui.project_panel.tabs.iter().position(|t| t.bin == bin.0).ok_or("bin view has closed")?;
    let owned: Vec<ItemId> = app
        .session
        .project
        .root
        .find_bin(bin)
        .into_iter()
        .flat_map(|b| &b.children)
        .filter_map(|entry| match entry {
            BinEntry::Item(id) if app.session.state.open_sequences.contains(id) => Some(*id),
            _ => None,
        })
        .collect();
    #[cfg(not(target_arch = "wasm32"))]
    app.session.prepare_closing_sequences(&owned).map_err(|e| e.to_string())?;
    if let Some(command) = decision
        && let Err(error) = app.session.execute(command, json!({"bin":bin}))
    {
        let message = match error {
            filmcraft_engine::EngineError::BinPublished { message, .. } => {
                tlf!("The bin was saved. Close it again to retry recovery cleanup. {message}", message)
            }
            other => other.to_string(),
        };
        app.session.error_toast(command, &message);
        app.ui.status = message.clone();
        return Err(message);
    }
    let name = app.session.project.root.find_bin(bin).map(|b| b.name.clone()).unwrap_or_else(|| bin.0.to_string());
    app.session.toast(tlf!("Closed bin {name}", name));
    app.session.close_sequence_views(&owned);
    super::remove_tab(app, k);
    app.bin_views.focused = None;
    app.bin_views.bring_forward =
        Some(app.ui.project_panel.active_tab.and_then(|k| app.ui.project_panel.tabs.get(k)).map_or(app.session.project.root.id.0, |t| t.bin));
    Ok(json!({"closed":true,"bin":bin}))
}

pub(crate) fn dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some((epoch, bin)) = app.bin_views.prompt else { return };
    if epoch != app.session.document_epoch || !app.ui.project_panel.tabs.iter().any(|t| t.bin == bin.0) {
        app.bin_views.prompt = None;
        return;
    }
    let name = app.session.project.root.find_bin(bin).map(|b| b.name.clone()).unwrap_or_else(|| bin.0.to_string());
    let title = tlf!("Save changes to {name}?", name);
    let mut decision = None;
    let modal = egui::Modal::new(egui::Id::unique("close-modular-bin"))
        .frame(crate::panels::file_dialogs::modal_frame(&app.tokens).inner_margin(egui::Margin::same(20)))
        .show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(&title);
            if app.session.bin_has_move_removal(bin) {
                ui.label(tl!(
                    "Saving removes the moved sequence from this bin. You can restore the cut as it left this bin from its move backup in project history."
                ));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (key, label, cmd) in [("save", tl!("Save"), "project.saveBin"), ("discard", tl!("Discard"), "project.discardBin")] {
                    let r = ui.add_sized([84.0, 28.0], egui::Button::new(label));
                    app.auto.add(&format!("project.closeBin.{key}"), r.rect, label);
                    if key == "save" && std::mem::take(&mut app.bin_views.focus_answer) {
                        r.request_focus();
                    }
                    if r.clicked() {
                        decision = Some(cmd);
                    }
                }
            });
        });
    ctx.accesskit_node_builder(modal.response.id, |node| {
        node.set_role(egui::accesskit::Role::Dialog);
        node.set_label(&title);
    });
    if let Some(command) = decision {
        app.bin_views.prompt = None;
        if finish(app, bin, Some(command)).is_err() {
            app.bin_views.bring_forward = Some(bin.0);
        }
    }
}
