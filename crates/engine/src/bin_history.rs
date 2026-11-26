//! Queries scoped to the open modular project. The revision is pinned by the caller.
use crate::{EngineError, Result, Session, project_panel};
use filmcraft_format::project_folder::{bin_history, bins};
use filmcraft_project::{BinEntry, ItemId};
use serde_json::{Value, json};
fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

pub(super) fn browse(s: &mut Session, p: &Value) -> Result<Value> {
    let folder = s.editing_folder.as_ref().ok_or_else(|| error("open a modular project first"))?.snapshot.directory();
    let bin = p.get("bin").map(|v| v.as_u64().ok_or_else(|| error("expected a bin ID"))).transpose()?;
    let Some(revision) = p.get("revision") else {
        return Ok(json!({"epoch":s.document_epoch,"revisions":bin_history::revisions(folder,bin).map_err(error)?}));
    };
    let revision = revision.as_str().ok_or_else(|| error("expected revision digest"))?;
    let Some(bin) = bin else {
        let mut listing = serde_json::to_value(bins::saved_bins(folder, revision).map_err(error)?).map_err(error)?;
        listing["epoch"] = s.document_epoch.into();
        return Ok(listing);
    };
    let mut project = bin_history::projection(folder, revision, bin).map_err(error)?;
    let mut saved_only = std::collections::BTreeSet::new();
    for (id, item) in &mut project.items {
        if !item.is_source_clip() {
            continue;
        }
        let Some(live) = s.project.item(*id) else {
            saved_only.insert(*id);
            continue;
        };
        item.name.clone_from(&live.name);
        item.label = live.label;
        item.custom_label_color = live.custom_label_color;
        item.metadata.retain(|key, _| project_panel::is_internal_key(key));
        item.metadata.extend(live.metadata.iter().filter(|(key, _)| !project_panel::is_internal_key(key)).map(|(key, value)| (key.clone(), value.clone())));
    }
    let columns: Vec<_> = s.prefs.project_panel.view.columns.iter().map(|c| &c.name).collect();
    let item_row = |id: ItemId, reference: bool| {
        let it = project.item(id)?;
        project_panel::listed(&project, it).then(
            || json!({"item":id,"name":it.name,"reference":reference,"savedMetadata":saved_only.contains(&id),"cells":columns.iter().map(|c|project_panel::cell(&project,it,c).0).collect::<Vec<_>>() }),
        )
    };
    let mut rows = Vec::new();
    for child in &project.root.children {
        match child {
            BinEntry::Bin(b) => rows.push(json!({"bin":b.id,"name":b.name})),
            BinEntry::Item(id) => rows.extend(item_row(*id, false)),
        }
    }
    rows.extend(project.root.filed_sources.iter().filter_map(|id| item_row(*id, false)));
    rows.extend(project_panel::reference_clips(&project, &project.root).into_iter().filter_map(|id| item_row(id, true)));
    Ok(json!({"epoch":s.document_epoch,"revision":revision,"bin":bin,"name":project.root.name,"columns":columns,"rows":rows}))
}

pub(super) fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    use crate::folder_edit::FolderBase;
    use filmcraft_format::project_folder::{EditOrigin, ROOT, history_paste};
    use std::sync::Arc;
    let bin = p["bin"].as_u64().ok_or_else(|| error("choose a destination bin"))?;
    let revision = p["revision"].as_str().ok_or_else(|| error("expected a saved revision"))?;
    let ids: Vec<u64> = serde::Deserialize::deserialize(&p["items"]).map_err(error)?;
    if p["epoch"].as_u64() != Some(s.document_epoch) {
        return Err(error("the copied items belong to a different document session"));
    }
    let view = s.project_view();
    let f = s.editing_folder.as_ref().ok_or_else(|| error("open a modular project first"))?;
    if !f.snapshot.writable() || f.snapshot.scope().is_some_and(|scope| scope.bin != bin) {
        return Err(error("open the destination bin for writing before pasting"));
    }
    let directory = f.snapshot.directory();
    let current = filmcraft_format::encode_with_view(&s.project, Some(&view), false);
    let raw = f.baseline.origin.checkpoint(directory, &f.baseline.typed(), &current, &directory.join(ROOT)).map_err(error)?;
    let raw = s.history_document(raw, &current)?;
    let next = if f.snapshot.is_shared() { f.snapshot.cursor() } else { s.project.next_id };
    let paste = history_paste::prepare(directory, revision, &ids, bin, &raw, next).map_err(error)?;
    let mut project = filmcraft_format::decode(&paste.document).map_err(error)?.project;
    // The stock reader advances its allocator past all visible IDs. A bin writer
    // must continue inside its own reservation even when a peer has higher IDs.
    if f.snapshot.scope().is_some() {
        project.next_id = paste.next_id;
    }
    if project != *s.project {
        s.edit("Paste from Bin History", |p, _| {
            *p = project;
            Ok(())
        })?;
        let base = FolderBase { origin: EditOrigin::Recovered(paste.document.into()), project: s.project.clone(), view };
        if let Some(folder) = &mut s.editing_folder {
            folder.baseline = Arc::new(base);
        }
    }
    Ok(json!({"items":paste.selected}))
}

pub(super) fn find_saved_item_uses(s: &mut Session, p: &Value) -> Result<Value> {
    let revision = p["revision"].as_str().ok_or_else(|| error("expected a saved revision"))?;
    let item = p["item"].as_u64().ok_or_else(|| error("expected an item ID"))?;
    let read = s.prepare_saved_item_uses(revision, item)?;
    let usage = read()?;
    serde_json::to_value(usage).map_err(error)
}

pub(super) fn compare_versions(s: &mut Session, p: &Value) -> Result<Value> {
    compare(s.history_folder()?, p)
}

impl Session {
    fn history_folder(&self) -> Result<&std::path::Path> {
        Ok(self.editing_folder.as_ref().ok_or_else(|| error("open a modular project first"))?.snapshot.directory())
    }

    /// Pin the search to this document and save before handing it to a worker.
    pub fn prepare_saved_item_uses(
        &self,
        revision: &str,
        item: u64,
    ) -> Result<impl FnOnce() -> Result<filmcraft_format::project_folder::usage::Usage> + Send + 'static + use<>> {
        let folder = self.history_folder()?.to_owned();
        let revision = revision.to_owned();
        Ok(move || filmcraft_format::project_folder::usage::find_uses(&folder, &revision, item).map_err(error))
    }

    /// Capture this document's location so a background reader never borrows the live session.
    pub fn prepare_bin_comparison(&self, params: Value) -> Result<impl FnOnce() -> Result<Value> + Send + 'static + use<>> {
        let folder = self.history_folder()?.to_owned();
        Ok(move || compare(&folder, &params))
    }
}

fn compare(folder: &std::path::Path, p: &Value) -> Result<Value> {
    use filmcraft_format::project_folder::compare::{self, Selection};
    let bin = p["bin"].as_u64().ok_or_else(|| error("expected a bin ID"))?;
    let to = p["to"].as_str().ok_or_else(|| error("expected a saved revision"))?;
    let mut result = json!({"bin":bin,"to":to});
    let selection = || -> Result<Selection> {
        Ok(match p.get("sequence") {
            None => Selection::Bin(bin),
            Some(v) => Selection::Sequence(v.as_u64().ok_or_else(|| error("expected a sequence ID"))?),
        })
    };
    let (from, report) = if let Some(v) = p.get("from") {
        let from = v.as_str().ok_or_else(|| error("expected a saved revision"))?;
        (from.to_owned(), compare::report(folder, from, to, selection()?).map_err(error)?)
    } else {
        let picker = compare::bin_versions(folder, to, bin).map_err(error)?;
        result["revisions"] = json!(picker.revisions);
        match picker.previous {
            Ok(Some(pair)) => pair.into_report(selection()?).map_err(error)?,
            Ok(None) => return Ok(result),
            Err(e) => {
                result["lookupError"] = e.to_string().into();
                return Ok(result);
            }
        }
    };
    result["report"] = serde_json::to_value(report).map_err(error)?;
    result["from"] = from.into();
    Ok(result)
}
