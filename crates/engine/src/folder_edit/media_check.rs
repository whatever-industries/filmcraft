//! Read the chosen saved bin and its source closure without changing the open edit.
use crate::{EngineError, Result, Session};
use filmcraft_format::project_folder::FolderDirectory;
use filmcraft_project::{BinId, DirectoryEntry, ItemId, MediaIdentity, MediaRef};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

impl Session {
    pub(crate) fn inspect_bin_media(&self, params: &Value) -> Result<Value> {
        let fail = |e: String| EngineError::Other(e);
        let id = BinId(params["bin"].as_u64().ok_or_else(|| fail("choose an editorial bin".into()))?);
        let path = Path::new(self.path.as_deref().ok_or_else(|| fail("open a modular project".into()))?);
        let revision = params["revision"].as_str().or_else(|| self.folder_revision()).ok_or_else(|| fail("choose a saved revision".into()))?;
        let directory = FolderDirectory::retained(path, revision).map_err(|e| fail(e.to_string()))?;
        let record = directory.bins.get(&id.0).ok_or_else(|| fail("unknown bin in selected revision".into()))?;
        let members = record["children"].as_array().ok_or_else(|| fail("missing bin entries".into()))?;
        let mut pending: Vec<_> = members.iter().filter_map(|entry| entry["Item"].as_u64().map(ItemId)).collect();
        let is_bin = if let Some(raw) = directory.envelope["project"].get("modular_directory") {
            let dir: filmcraft_project::ProjectDirectory = serde_json::from_value(raw.clone()).map_err(|e| fail(e.to_string()))?;
            dir.validate(&directory.bins.keys().copied().map(BinId).collect()).map_err(fail)?;
            dir.parent(DirectoryEntry::Bin(id)).is_some()
        } else {
            !pending.is_empty() || members.is_empty() && directory.envelope["project"]["root"] != id.0
        };
        if !is_bin {
            return Err(fail("choose an editorial bin".into()));
        }
        let mut files = Vec::new();
        let mut visited = BTreeSet::new();
        let mut remaining = directory.remaining_item_bytes;
        let inspect = |stored: &str, identity: Option<MediaIdentity>| {
            let resolved = match crate::doc_path::resolve(stored, Some(path)) {
                Ok(p) => p,
                Err(_) => return "refused",
            };
            match crate::relink::identity_of(&*self.services, &resolved) {
                Ok(actual) => match identity {
                    Some(saved) if saved == actual => "matched",
                    Some(_) => "changed",
                    None => "presentUnverified",
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
                Err(_) => "unreadable",
            }
        };
        while let Some(item) = pending.pop() {
            if !visited.insert(item) {
                continue;
            }
            let record = directory.read_item(item, &mut remaining).map_err(|e| fail(e.to_string()))?;
            record.visit_source_items(|source| pending.push(source));
            if let Some(media) = record.as_media()
                && let MediaRef::File { path } = &media.media
            {
                let proxy = match &media.proxy {
                    Some(MediaRef::File { path }) => Some(inspect(path, None)),
                    _ => None,
                };
                files.push(json!({"item":item,"name":record.name,"original":inspect(path,media.identity),"proxy":proxy,"offline":media.offline}));
            }
        }
        Ok(json!({"revision":revision,"bin":id,"inspectedItems":visited,"media":files}))
    }
}
