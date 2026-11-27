//! Headless discovery on a pinned saved revision. No partially loaded session accepts edits.
use super::{Index, Reader, RecordClass, Result, cache};
use filmcraft_engine::{Session, project_tools::query_p};
use filmcraft_project::{Bin, BinEntry, BinId, FindQuery, ItemId, Project};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path, sync::Arc, time::Instant};

pub(super) struct Discovery {
    pub(super) session: Session,
    pub(super) reader: Reader,
    pub(super) index: Index,
    pub(super) remaining: u64,
}

impl Discovery {
    pub(super) fn new(path: &Path, mut reader: Reader, index: Index) -> Result<Self> {
        let mut session = Session::default();
        session.project = Arc::new(reader.take_project()?);
        session.path = Some(path.to_str().ok_or("invalid project path")?.to_owned());
        let remaining = reader.directory.remaining_item_bytes;
        Ok(Self { session, reader, index, remaining })
    }

    fn load(&mut self, include: impl Fn(RecordClass) -> bool) -> Result<()> {
        let mut ids = Vec::new();
        for (&id, label) in &self.index.labels {
            if include(label.class.ok_or("incomplete discovery index")?) {
                ids.push(id);
            }
        }
        self.load_ids(ids)
    }

    pub(super) fn load_ids(&mut self, ids: impl IntoIterator<Item = ItemId>) -> Result<()> {
        let missing: Vec<_> = ids.into_iter().filter(|id| !self.session.project.items.contains_key(id)).collect();
        let mut remaining = self.remaining;
        self.read_ids(missing, &mut remaining)?;
        self.remaining = remaining;
        Ok(())
    }

    // Read and verify all requested records before installing any of them. Apply calls
    // this under its revision lock, even for records previously hydrated for playback.
    pub(super) fn read_ids(&mut self, ids: impl IntoIterator<Item = ItemId>, remaining: &mut u64) -> Result<()> {
        let mut records = Vec::new();
        for id in ids {
            let item =
                self.reader.directory.read_item(id, remaining).map_err(|error| format!("incomplete discovery: could not read item {}: {error}", id.0))?;
            records.push((id, item));
        }
        Arc::make_mut(&mut self.session.project).items.extend(records);
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

// Use the editor's missing-media scan, candidate ordering, fingerprint checks and stored-path policy.
fn relink_candidates(session: &Session, params: &Value, revision: Option<&str>) -> Result<Value> {
    let missing = filmcraft_engine::relink::scan(&session.project, &session.media, &*session.services);
    let mut report = filmcraft_engine::relink::search(session, params)?;
    if let Some(revision) = revision {
        filmcraft_engine::relink::review::annotate(&session.project, revision, &mut report);
    }
    report["missing"] = json!(missing);
    Ok(report)
}

fn full_search(project: &Project, query: &FindQuery) -> Value {
    // Attach the actual bin at each occurrence in the full editor's ordered results.
    let hits = query.find(project);
    let mut hits = hits.into_iter().peekable();
    let entries = panel_order(&project.root, project.items.keys().copied());
    json!(entries.into_iter().filter(|(id, _)| hits.next_if_eq(id).is_some()).collect::<Vec<_>>())
}

pub(in super::super) fn run(mode: &str, path: &Path, params: &str) -> Result<()> {
    let params: Value = serde_json::from_str(params)?;
    let query = match mode {
        "search" => Some(query_p(&params, mode)?),
        "relink-discovery" => None,
        _ => return Err("unknown discovery command".into()),
    };
    let start = Instant::now();
    let (results, project_bytes, decoded, cache_written) = if path.extension().is_some_and(|ext| ext == "fcmod") {
        let data = filmcraft_engine::autosave::default_data_dir().ok_or("no FilmCraft user data directory")?;
        let (mut view, cost) = cache::open_with(path, &data, || Ok(()), Discovery::new).map_err(|e| format!("incomplete {mode}: {e}"))?;
        let results = match &query {
            Some(query) => view.search(query)?,
            None => {
                view.load(|class| class == RecordClass::Media)?;
                relink_candidates(&view.session, &params, Some(&view.reader.directory.revision))?
            }
        };
        (
            results,
            cost.project_bytes + (view.reader.directory.remaining_item_bytes - view.remaining),
            cost.records + view.session.project.items.len(),
            cost.cache_written,
        )
    } else {
        let mut session = Session::local_folder_editor();
        session.execute("file.open", json!({"path":path}))?;
        let results = match &query {
            Some(query) => full_search(&session.project, query),
            None => relink_candidates(&session, &params, None)?,
        };
        (results, fs::metadata(path)?.len(), session.project.items.len(), 0)
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
    let rows: Vec<(ItemId, Option<BinId>)> = serde_json::from_value(results.clone())?;
    assert_eq!(rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(), query.find(&reference.project));
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

#[cfg(test)]
mod relink_tests {
    use super::*;
    use crate::native::preview::tests::fixture;

    #[test]
    fn moved_media_is_ranked_across_unopened_bins_without_changing_the_project() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let f = fixture(tmp.path())?;
        let media = tmp.path().join("Media");
        fs::create_dir(&media)?;
        let mut editor = Session::local_folder_editor();
        editor.execute("file.open", json!({"path":f.path}))?;
        let mut ids = Vec::new();
        for (name, bin, color) in [("hero.png", f.cameras, 32), ("lost.png", f.other, 96)] {
            let path = media.join(name);
            image::RgbImage::from_pixel(12, 8, image::Rgb([color, 17, 0])).save(&path)?;
            let imported = editor.execute("file.import", json!({"paths":[path],"bin":bin}))?;
            let id: ItemId = serde_json::from_value(imported["items"][0].clone())?;
            ids.push(id);
        }
        editor.execute("file.save", json!({}))?;
        let saved = fs::read(&f.path)?;
        let moved = tmp.path().join("Moved");
        fs::rename(media, &moved)?;
        fs::rename(moved.join("lost.png"), tmp.path().join("aside.png"))?;
        let wrong = moved.join("different");
        fs::create_dir(&wrong)?;
        image::RgbImage::from_pixel(12, 8, image::Rgb([128u8, 1, 4])).save(wrong.join("hero.png"))?;
        let params = json!({"folder":moved});
        let mut full = Session::local_folder_editor();
        full.execute("file.open", json!({"path":f.path}))?;
        let expected = full.execute("media.search", params.clone())?;
        let (mut view, _) = cache::open_with(&f.path, &tmp.path().join("data"), || Ok(()), Discovery::new)?;
        view.load(|class| class == RecordClass::Media)?;
        assert!(!view.session.project.items.contains_key(&f.master));
        let paths_before = view.session.project.clone();
        let report = relink_candidates(&view.session, &params, Some(&view.reader.directory.revision))?;
        assert_eq!(report["results"], expected["results"]);
        assert_eq!(report["missing"], json!(filmcraft_engine::relink::scan(&full.project, &full.media, &*full.services)));
        assert_eq!(report["missing"].as_array().unwrap().len(), 2);
        let rows = report["results"].as_array().unwrap();
        let hero = rows.iter().find(|r| r["item"] == json!(ids[0])).unwrap()["candidates"].as_array().unwrap();
        assert_eq!(hero.len(), 2);
        assert_eq!(hero[0]["ok"], true);
        assert_eq!(hero[0]["identityMatch"], true);
        assert_eq!(hero[1]["ok"], false);
        assert_eq!(hero[1]["identityMatch"], false);
        assert!(!hero[1]["problems"].as_array().unwrap().is_empty());
        assert_eq!(rows.iter().find(|r| r["item"] == json!(ids[1])).unwrap()["candidates"], json!([]));
        assert!(Arc::ptr_eq(&paths_before, &view.session.project));
        assert!(!view.session.is_dirty());
        assert!(view.session.dirty_folder_bins().is_empty());
        assert_eq!(fs::read(&f.path)?, saved);
        Ok(())
    }
}
