//! Headless discovery on a pinned saved revision. No partially loaded session accepts edits.
use super::{Index, Reader, RecordClass, Result, cache};
use filmcraft_engine::{Session, project_tools::query_p};
use filmcraft_project::{Bin, BinEntry, BinId, FindQuery, ItemId, Project};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path, sync::Arc, time::Instant};

struct Discovery {
    session: Session,
    reader: Reader,
    index: Index,
    remaining: u64,
}

impl Discovery {
    fn new(path: &Path, mut reader: Reader, index: Index) -> Result<Self> {
        let mut session = Session::default();
        session.project = Arc::new(reader.take_project()?);
        session.path = Some(path.to_str().ok_or("invalid project path")?.to_owned());
        let remaining = reader.directory.remaining_item_bytes;
        Ok(Self { session, reader, index, remaining })
    }

    fn load(&mut self, include: impl Fn(RecordClass) -> bool) -> Result<()> {
        let items = &mut Arc::make_mut(&mut self.session.project).items;
        for (&id, label) in &self.index.labels {
            if include(label.class.ok_or("incomplete discovery index")?)
                && let std::collections::btree_map::Entry::Vacant(entry) = items.entry(id)
            {
                let item = self
                    .reader
                    .directory
                    .read_item(id, &mut self.remaining)
                    .map_err(|error| format!("incomplete discovery: could not read item {}: {error}", id.0))?;
                entry.insert(item);
            }
        }
        self.session.media.add_stored(&self.session.project, self.session.path.as_deref());
        filmcraft_engine::still_migration::migrate_legacy_still_sizes(Arc::make_mut(&mut self.session.project), &self.session.media, &*self.session.services);
        Ok(())
    }

    fn search(&mut self, query: &FindQuery) -> Result<Value> {
        if let Some(matches) = query.name_matcher() {
            let order = panel_order(&self.session.project.root, self.index.labels.keys().copied());
            return Ok(json!(
                order
                    .into_iter()
                    .filter(|(id, _)| {
                        let label = &self.index.labels[id];
                        label.class != Some(RecordClass::Graphic) && matches(&label.name)
                    })
                    .collect::<Vec<_>>()
            ));
        }
        self.load(|class| class != RecordClass::Graphic)?;
        Ok(full_search(&self.session.project, query))
    }
}

// Preserve every bin entry in panel traversal order, followed by unfiled items in ID order.
fn panel_order(root: &Bin, ids: impl Iterator<Item = ItemId>) -> Vec<(ItemId, Option<BinId>)> {
    fn walk(bin: &Bin, entries: &mut Vec<(ItemId, Option<BinId>)>) {
        for entry in &bin.children {
            match entry {
                BinEntry::Bin(child) => walk(child, entries),
                BinEntry::Item(id) => entries.push((*id, Some(bin.id))),
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, &mut entries);
    let filed: BTreeSet<_> = entries.iter().map(|(id, _)| *id).collect();
    entries.extend(ids.filter(|id| !filed.contains(id)).map(|id| (id, None)));
    entries
}

fn full_search(project: &Project, query: &FindQuery) -> Value {
    json!(query.find(project).into_iter().map(|id| (id, project.root.parent_of(id))).collect::<Vec<_>>())
}

pub(in super::super) fn run(mode: &str, path: &Path, params: &str) -> Result<()> {
    let params: Value = serde_json::from_str(params)?;
    let query = query_p(&params, mode)?;
    let start = Instant::now();
    let (results, project_bytes, decoded, cache_written) = if path.extension().is_some_and(|ext| ext == "fcmod") {
        let data = filmcraft_engine::autosave::default_data_dir().ok_or("no FilmCraft user data directory")?;
        let (mut view, cost) = cache::open_with(path, &data, || Ok(()), Discovery::new)?;
        let results = view.search(&query)?;
        (
            results,
            cost.project_bytes + (view.reader.directory.remaining_item_bytes - view.remaining),
            cost.records + view.session.project.items.len(),
            cost.cache_written,
        )
    } else {
        let mut session = Session::local_folder_editor();
        session.execute("file.open", json!({"path":path}))?;
        (full_search(&session.project, &query), fs::metadata(path)?.len(), session.project.items.len(), 0)
    };
    println!(
        "{}",
        json!({"results":results, "projectBytes":project_bytes,"decoded":decoded,"cacheBytesWritten":cache_written,"ms":start.elapsed().as_secs_f64()*1000.0})
    );
    Ok(())
}

#[cfg(test)]
pub(super) fn check_saved_search(path: &Path, reference: &Session, cache_dir: &Path, query: &FindQuery) -> Result<Value> {
    let (mut view, _) = cache::open_with(path, cache_dir, || Ok(()), Discovery::new)?;
    let results = view.search(query)?;
    assert_eq!(results, full_search(&reference.project, query));
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::preview::tests::fixture;
    use filmcraft_project::{FindOp, ItemKind};
    use filmcraft_time::{Tick, TimeRange};

    #[test]
    fn unopened_names_and_metadata_match_full_find_and_damage_is_explicit() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let f = fixture(temp.path())?;
        let mut editor = Session::local_folder_editor();
        editor.execute("file.open", json!({"path":f.path}))?;
        let (sub, hidden) = editor.edit("Prepare discovery fixture", |p, _| {
            let label = p.items[&f.camera].label;
            p.item_mut(f.other_camera).unwrap().name = "Camera".into();
            p.item_mut(f.other_camera).unwrap().metadata.insert("Scene".into(), "Night".into());
            let sub = p.add_item(
                "Camera",
                label,
                ItemKind::Subclip { parent: f.camera, range: TimeRange::new(Tick::ZERO, Tick(1000)), restrict_trims: false },
                Some(f.other),
            );
            let hidden = p.add_item("Camera", label, ItemKind::Graphic { width: 128, height: 72, rate: Default::default() }, Some(f.other));
            Ok((sub, hidden))
        })?;
        editor.execute("file.save", json!({}))?;
        let mut reference = Session::local_folder_editor();
        reference.execute("file.open", json!({"path":f.path}))?;
        let data = temp.path().join("data");
        let (mut view, _) = cache::open_with(&f.path, &data, || Ok(()), Discovery::new)?;
        let names = FindQuery::simple("Name", FindOp::Matches, "camera");
        let found = view.search(&names)?;
        assert_eq!(found, full_search(&reference.project, &names));
        let hits = found.as_array().unwrap();
        assert_eq!(hits.len(), 3);
        assert!(hits.contains(&json!([sub, f.other])));
        assert!(!hits.contains(&json!([hidden, f.other])));
        assert!(view.session.project.items.is_empty());
        for (column, text, expected) in [("Video Info", "128 x 72", sub), ("Scene", "Night", f.other_camera)] {
            let q = FindQuery::simple(column, FindOp::Contains, text);
            let hits = check_saved_search(&f.path, &reference, &data, &q)?;
            assert!(hits.as_array().unwrap().contains(&json!([expected, f.other])));
        }
        let manifest: Value = serde_json::from_slice(&fs::read(&f.path)?)?;
        let records: Vec<(ItemId, String)> = serde_json::from_value(manifest["items"].clone())?;
        let (_, hash) = records.iter().find(|(id, _)| *id == f.other_camera).unwrap();
        fs::remove_file(f.path.with_file_name(hash))?;
        let (mut warm, cost) = cache::open_with(&f.path, &data, || Ok(()), Discovery::new)?;
        assert_eq!(cost.records, 0);
        assert_eq!(warm.search(&names)?, found);
        assert!(warm.search(&FindQuery::simple("Scene", FindOp::Matches, "Night")).unwrap_err().to_string().contains("incomplete discovery"));
        Ok(())
    }
}
