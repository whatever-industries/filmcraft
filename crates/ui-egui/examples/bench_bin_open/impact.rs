//! Read-only impact qualification against a pinned saved modular project.
use super::{Result, cache, discovery::Discovery};
use filmcraft_engine::relink::{
    identity_of,
    review::{self, Review},
};
use filmcraft_format::project_folder::FolderDirectory;
use filmcraft_project::ItemId;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

pub(super) fn check(path: &Path, data: &Path, candidate: &Review, before_recheck: impl FnOnce() -> Result<()>) -> Result<Value> {
    let start = Instant::now();
    let (mut view, cost) = cache::open_with(path, data, || Ok(()), Discovery::new)?;
    if candidate.revision != view.reader.directory.revision {
        return Err("project changed; refresh discovery".into());
    }
    view.load_ids(view.index.impact_items(candidate.item)?)?;
    let result = review::impact(&view.session, &view.reader.directory.revision, candidate)?;
    before_recheck()?;
    if identity_of(&*view.session.services, &candidate.path).ok() != Some(candidate.identity) {
        return Err("candidate changed during impact check; refresh discovery".into());
    }
    let (revision, bytes) = FolderDirectory::published_revision_size(path)?;
    if revision != candidate.revision {
        return Err("project changed during impact check; refresh discovery".into());
    }
    Ok(json!({"results":result,"projectBytes":cost.project_bytes + view.reader.directory.remaining_item_bytes - view.remaining + bytes,
        "decoded":cost.records + view.session.project.items.len(),"cacheBytesWritten":cost.cache_written,"ms":start.elapsed().as_secs_f64()*1000.0}))
}

pub(in super::super) fn run(path: &Path, input: &str) -> Result<()> {
    let data = filmcraft_engine::autosave::default_data_dir().ok_or("no FilmCraft user data directory")?;
    println!("{}", check(path, &data, &serde_json::from_str(input)?, || Ok(()))?);
    Ok(())
}

/// Comparison for the paired standard fixture produced by `prepare`. A standard project has
/// no modular revision of its own. Only compare the result after checking the fixture pair.
pub(in super::super) fn baseline(path: &Path, input: &str) -> Result<()> {
    if path.extension().is_none_or(|ext| ext != "fcproj") {
        return Err("expected the paired standard .fcproj fixture".into());
    }
    let candidate: Review = serde_json::from_str(input)?;
    let start = Instant::now();
    let mut editor = filmcraft_engine::Session::default();
    editor.execute("file.open", json!({"path":path}))?;
    let result = review::impact(&editor, &candidate.revision, &candidate)?;
    if identity_of(&*editor.services, &candidate.path)? != candidate.identity {
        return Err("candidate changed during full check".into());
    }
    println!(
        "{}",
        json!({"results":result, "ms":start.elapsed().as_secs_f64()*1000.0,
        "projectBytes":std::fs::metadata(path)?.len(),"decoded":editor.project.items.len(),"cacheBytesWritten":0})
    );
    Ok(())
}

/// Run only on a disposable modular fixture. The normal editor may acquire its writer lock.
pub(in super::super) fn verify(path: &Path, input: &str) -> Result<()> {
    let review: Review = serde_json::from_str(input)?;
    let data = tempfile::tempdir()?;
    let prediction = check(path, data.path(), &review, || Ok(()))?;
    compare_editor(path, &review, &prediction["results"])?;
    println!("{prediction}");
    Ok(())
}

fn compare_editor(path: &Path, review: &Review, predicted: &Value) -> Result<()> {
    use filmcraft_engine::Session;
    use filmcraft_project::{Bin, BinEntry, BinId};
    let mut editor = Session::local_folder_editor();
    let opened = editor.execute("file.open", json!({"path":path}))?;
    if opened["folderRevision"] != review.revision {
        return Err("oracle reopened a different revision".into());
    }
    editor.execute("project.select", json!({"items":[review.item]}))?;
    let before = editor.project.clone();
    let applied = editor.execute("media.relink", json!({"item":review.item,"bin":review.bin,"path":review.path,"fullEditorReview":review}));
    let refused = applied.is_err();
    if predicted["status"] == "script-review" {
        let error = applied.err().ok_or("the editor accepted a relink predicted to require Script review")?;
        if !error.to_string().contains(filmcraft_engine::relink::SCRIPT_MARKS_RELINK_REVIEW) || editor.is_dirty() {
            return Err(error.into());
        }
    } else {
        applied?;
    }
    if editor.project.root != before.root || editor.project.items.keys().ne(before.items.keys()) {
        return Err("relink changed the bin tree or inventory".into());
    }
    let mut changed = Vec::new();
    let mut owners: BTreeMap<ItemId, BTreeSet<BinId>> = BTreeMap::new();
    let mut unfiled = Vec::new();
    fn collect(bin: &Bin, id: ItemId, owners: &mut BTreeSet<BinId>) {
        for entry in &bin.children {
            match entry {
                BinEntry::Bin(child) => collect(child, id, owners),
                BinEntry::Item(item) if *item == id => {
                    owners.insert(bin.id);
                }
                _ => {}
            }
        }
    }
    for (&id, item) in &editor.project.items {
        if before.item(id) != Some(item) {
            changed.push(id);
            let bins = owners.entry(id).or_default();
            collect(&editor.project.root, id, bins);
            if bins.is_empty() {
                unfiled.push(id);
            }
        }
    }
    let status = editor.execute("project.binStatus", json!({}))?;
    let bins: BTreeSet<_> = owners.values().flatten().collect();
    let classification = if refused {
        "script-review"
    } else if changed.is_empty() {
        "unchanged"
    } else if changed == [review.item] {
        "source-only"
    } else {
        "full-editor"
    };
    if predicted["status"] != classification
        || predicted["changed"] != json!(changed)
        || predicted["unfiled"] != json!(unfiled)
        || status["dirtyBins"] != json!(bins)
        || status["unfiledItemsDirty"] != !unfiled.is_empty()
        || status["projectSettingsDirty"] != false
        || changed.iter().any(|id| predicted["owners"][id.0.to_string()] != json!(owners[id]))
    {
        return Err(format!("impact prediction {predicted} differs from editor records {changed:?}, owners {owners:?}, status {status}").into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "impact_tests.rs"]
mod tests;
