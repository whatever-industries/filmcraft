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
        return Ok(json!({"revisions":bin_history::revisions(folder,bin).map_err(error)?}));
    };
    let revision = revision.as_str().ok_or_else(|| error("expected revision digest"))?;
    let Some(bin) = bin else {
        return serde_json::to_value(bins::saved_bins(folder, revision).map_err(error)?).map_err(error);
    };
    let project = bin_history::projection(folder, revision, bin).map_err(error)?;
    let columns: Vec<_> = s.prefs.project_panel.view.columns.iter().map(|c| &c.name).collect();
    let item_row = |id: ItemId, reference: bool| {
        let it = project.item(id)?;
        project_panel::listed(&project, it).then(
            || json!({"item":id,"name":it.name,"reference":reference,"cells":columns.iter().map(|c|project_panel::cell(&project,it,c).0).collect::<Vec<_>>() }),
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
    Ok(json!({"revision":revision,"bin":bin,"name":project.root.name,"columns":columns,"rows":rows}))
}
