//! Bring a project-wide match into its real owning bin before accepting it.
use crate::{FilmcraftApp, panels::project, state::LinkMediaDraft};
use filmcraft_engine::relink::review::Review;
use serde_json::json;

pub(super) fn open(app: &mut FilmcraftApp, draft: &mut LinkMediaDraft, review: Review) -> Result<(), String> {
    if app.session.folder_revision() != Some(&review.revision) || app.session.project.root.parent_of(review.item) != Some(review.bin) {
        return Err(tl!("Source or project changed. Search again.").into());
    }
    project::open_bin(app, project::Inst::Main, review.bin.0, Some("openInPlace"), egui::Modifiers::NONE)?;
    app.session.execute("project.select", json!({"items":[review.item]})).map_err(|e| e.to_string())?;
    draft.focus_relink_review = true;
    draft.full_editor_reason = None;
    draft.pending_bin_review = Some((app.session.document_epoch, review));
    Ok(())
}

pub(super) fn confirm(app: &mut FilmcraftApp, ctx: &egui::Context, draft: &mut LinkMediaDraft, epoch: u64, review: Review) {
    let mut accept = false;
    let mut back = false;
    if app.session.document_epoch != epoch || draft.full_editor_reason.as_ref().is_some_and(|(rev, _)| *rev != app.session.revision) {
        draft.message = tl!("The project changed. Search again and review the source.").into();
        draft.bin_reviews.clear();
        draft.candidates.clear();
        draft.candidate = None;
        return;
    }
    let selected = project::focused_bin(app) == review.bin && app.session.state.project_selection.as_slice() == [review.item];
    let full_editor = draft.full_editor_reason.is_some();
    let modal = egui::Modal::new(egui::Id::unique("verified-bin-relink")).show(ctx, |ui| {
        ui.heading(tl!("Review Source Relink"));
        if let (Some(bin), Some(item)) = (app.session.project.root.find_bin(review.bin), app.session.project.item(review.item)) {
            ui.label(tlf!("Source: {clip} in {bin}", clip = &item.name, bin = &bin.name));
        }
        ui.label(&review.path);
        if let Some((_, reason)) = &draft.full_editor_reason {
            ui.label(reason);
        }
        if !selected {
            ui.label(tl!("Select the source in its owning bin to apply this relink."));
        }
        ui.horizontal(|ui| {
            let r = ui.button(tl!("Back"));
            app.auto.add("linkMedia.review.back", r.rect, tl!("Back"));
            if std::mem::take(&mut draft.focus_relink_review) {
                r.request_focus();
            }
            back = r.clicked();
            let label = if full_editor { tl!("Relink in Full Editor") } else { tl!("Apply Relink") };
            let r = ui.add_enabled(selected, egui::Button::new(label));
            app.auto.add("linkMedia.review.apply", r.rect, label);
            accept = r.clicked();
        });
    });
    ctx.accesskit_node_builder(modal.response.id, |node| {
        node.set_role(egui::accesskit::Role::Dialog);
        node.set_label(tl!("Review Source Relink"));
    });
    if accept && selected {
        let mut params = json!({"bin":review.bin,"item":review.item,"path":review.path});
        params[if full_editor { "fullEditorReview" } else { "review" }] = json!(review);
        match app.session.execute("media.relink", params) {
            Ok(v) => super::set_relink_result(app, draft, &v),
            Err(filmcraft_engine::EngineError::RelinkRequiresFullEditor { records }) => {
                draft.full_editor_reason = Some((
                    app.session.revision,
                    tlf!("Other records would change: {records}. Review these edits before continuing in the full editor.", records = records),
                ));
                draft.focus_relink_review = true;
                draft.pending_bin_review = Some((epoch, review));
            }
            Err(e) => draft.message = e.to_string(),
        }
        draft.bin_reviews.clear();
        draft.candidates.clear();
        draft.candidate = None;
    } else if !back && !modal.should_close() {
        draft.pending_bin_review = Some((epoch, review));
    }
}
