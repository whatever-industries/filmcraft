//! Historical cuts enter the working project through one undoable edit.
use crate::{EngineError, Result, Session};
use filmcraft_format::project_folder as storage;
use filmcraft_project::{BinEntry, ItemId, Project};
use serde_json::{Value, json};
use std::path::Path;

fn error(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

fn version(p: &Value) -> Result<&str> {
    p["revision"].as_str().ok_or_else(|| error("expected a backup revision"))
}

fn sequence(p: &Value) -> Result<ItemId> {
    p["sequence"].as_u64().map(ItemId).ok_or_else(|| error("expected a sequence ID"))
}

impl Session {
    pub(crate) fn recovery_versions(&mut self, p: &Value) -> Result<Value> {
        let (folder, _) = self.recovery_folder()?;
        let before = p.get("before").map(|n| n.as_u64().ok_or_else(|| error("expected a save number"))).transpose()?;
        storage::recovery_versions(folder, before).map_err(error)
    }

    pub(crate) fn recovery_sequences(&mut self, p: &Value) -> Result<Value> {
        let (folder, _) = self.recovery_folder()?;
        let filter = p.get("sequence").map(|_| sequence(p)).transpose()?.map(|id| id.0);
        Ok(json!({"sequences":storage::historical_sequences(folder, version(p)?, filter).map_err(error)?}))
    }

    pub(crate) fn export_recovery_sequence(&mut self, p: &Value) -> Result<Value> {
        let (folder, _) = self.recovery_folder()?;
        let path = p["path"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| error("choose a new project file"))?;
        let bytes = storage::rescue_bytes(folder, version(p)?, sequence(p)?.0, Path::new(path)).map_err(error)?;
        self.services.write_file_new(path, &bytes).map_err(error)?;
        Ok(json!({"path":path,"bytes":bytes.len()}))
    }

    pub(crate) fn restore_sequence_copy(&mut self, p: &Value) -> Result<Value> {
        let (folder, head) = self.recovery_folder()?;
        let source = sequence(p)?;
        match storage::editor_restore(folder, head, version(p)?, source.0, &self.project).map_err(error)? {
            storage::EditorRestore::ExportRequired(reason) => Ok(json!({"restored":false,"exportRequired":true,"reason":reason})),
            storage::EditorRestore::Copy(mut cut) => self.edit("Restore Sequence as Copy", |project, _| {
                let bin = project.root.parent_of(source).unwrap_or(project.root.id);
                let mut item = cut.items.remove(&source).ok_or_else(|| error("missing historical sequence"))?;
                let id = ItemId(project.next_id.max(cut.next_id));
                if id.0 >= Project::MAX_NEXT_ID {
                    return Err(error("project ID limit reached"));
                }
                project.next_id = id.0 + 1;
                item.id = id;
                item.created = id.0;
                item.name = format!("{} (Recovered)", item.name);
                let name = item.name.clone();
                project.items.insert(id, item);
                project.root.find_bin_mut(bin).ok_or_else(|| error("missing destination bin"))?.children.push(BinEntry::Item(id));
                if let Some(v) = cut.transcripts.remove(&source) {
                    project.transcripts.insert(id, v);
                }
                if let Some(v) = cut.narrations.remove(&source) {
                    project.narrations.insert(id, v);
                }
                if let Some(v) = cut.source_graphics.remove(&source) {
                    project.source_graphics.insert(id, v);
                }
                Ok(json!({"restored":true,"item":id,"bin":bin,"name":name}))
            }),
        }
    }
}
