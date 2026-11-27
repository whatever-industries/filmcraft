use super::*;
use crate::native::preview::tests::fixture;
use filmcraft_engine::Session;
use filmcraft_format::{atomic_write_new, encode_with_view, project_folder};
use filmcraft_project::{BinEntry, ItemKind, Label, ParamValue};
use filmcraft_time::{Tick, TimeRange};
use std::{fs, path::PathBuf, sync::Arc};

#[derive(Clone, Copy)]
enum Case {
    Plain,
    Dependent,
    Marked,
}

struct Cut {
    manifest: PathBuf,
    review: Review,
    nested: ItemId,
    other: ItemId,
    master: ItemId,
    script: ItemId,
}

fn cut(dir: &Path, case: Case) -> Result<Cut> {
    let f = fixture(dir)?;
    let mut s = Session::default();
    s.execute("file.open", json!({"path":dir.join("source.fcproj")}))?;
    let old = dir.join("Originals");
    fs::create_dir(&old)?;
    let png = old.join("camera.png");
    image::RgbImage::from_pixel(64, 32, image::Rgb([32u8, 67, 98])).save(&png)?;
    let imported = s.execute("file.import", json!({"paths":[png],"bin":f.cameras}))?;
    let source: ItemId = serde_json::from_value(imported["items"][0].clone())?;
    let p = Arc::make_mut(&mut s.project);
    let range = TimeRange::new(Tick::ZERO, Tick(1000));
    let inner = p.add_item("Subclip", Label::Iris, ItemKind::Subclip { parent: source, range, restrict_trims: false }, Some(f.cameras));
    let outer = p.add_item("Select", Label::Iris, ItemKind::Subclip { parent: inner, range, restrict_trims: false }, Some(f.other));
    p.sequence_mut(f.nested).unwrap().video_tracks[0].items[0].item = outer;
    p.sequence_mut(f.master).unwrap().audio_tracks[0].items[0].item = source;
    p.resolve_placed_auto_points(|_| true, |_| true);
    let script: ItemId = serde_json::from_value(s.execute("script.importText", json!({"name":"Pages","text":"Action","bin":f.other}))?["item"].clone())?;
    let line = s.project.item(script).unwrap().as_script().unwrap().lines[0].id;
    let take = s.execute("script.addTake", json!({"item":script,"source":outer,"startLine":line,"endLine":line}))?;
    if matches!(case, Case::Plain | Case::Marked) {
        s.execute("script.addMark", json!({"item":script,"take":take["take"],"line":line,"sourceTime":0}))?;
    }
    if !matches!(case, Case::Plain) {
        let p = Arc::make_mut(&mut s.project);
        p.item_mut(script).unwrap().as_script_mut().unwrap().slates[0].takes[0].source_identity = None;
        p.root.remove_item(script);
        p.root.find_bin_mut(f.cameras).unwrap().children.push(BinEntry::Item(f.deferred));
        for (sequence, key) in [(f.nested, "anchor"), (f.deferred, "position")] {
            let clip = &mut p.sequence_mut(sequence).unwrap().video_tracks[0].items[0];
            let value = &mut clip.effect_mut("motion").unwrap().param_mut(key).unwrap().value;
            let ParamValue::Vec2(point) = value else { panic!("expected a point") };
            point.x = f64::NAN;
        }
    }
    let dir_project = dir.join("impact.fcproj");
    atomic_write_new(&dir_project, &encode_with_view(&s.project, Some(&s.project_view()), false))?;
    let modular = dir.join("impact");
    project_folder::pack(&dir_project, &modular, atomic_write_new)?;
    let manifest = modular.join(project_folder::ROOT);
    let moved = dir.join("Moved");
    fs::rename(old, &moved)?;
    let mut full = Session::local_folder_editor();
    full.execute("file.open", json!({"path":manifest}))?;
    let report = full.execute("media.search", json!({"item":source,"folder":moved}))?;
    let review = serde_json::from_value(report["results"][0]["candidates"][0]["review"].clone())?;
    Ok(Cut { manifest, review, nested: f.nested, other: f.deferred, master: f.master, script })
}

#[test]
fn predicted_records_and_bins_match_a_real_edit_on_the_same_revision() -> Result<()> {
    for case in [Case::Plain, Case::Dependent, Case::Marked] {
        let temp = tempfile::tempdir()?;
        let f = cut(temp.path(), case)?;
        let manifest = fs::read(&f.manifest)?;
        let predicted = check(&f.manifest, &temp.path().join("cache"), &f.review, || Ok(()))?;
        let result = &predicted["results"];
        let changed: BTreeSet<ItemId> = serde_json::from_value(result["changed"].clone())?;
        match case {
            Case::Plain => {
                assert_eq!(result["status"], "source-only");
                assert_eq!(changed, [f.review.item].into());
            }
            Case::Dependent => {
                assert_eq!(result["status"], "full-editor");
                assert_eq!(changed, [f.review.item, f.nested, f.other, f.script].into());
                assert_eq!(result["unfiled"], json!([f.script]));
                assert_eq!(result["owners"][f.other.0.to_string()].as_array().unwrap().len(), 2);
            }
            Case::Marked => {
                assert_eq!(result["status"], "script-review");
                assert_eq!(result["scriptReview"], json!([f.script]));
                assert!(changed.is_empty());
            }
        }
        assert!(!changed.contains(&f.master));
        let mut open = Session::local_folder_editor();
        open.execute("file.open", json!({"path":f.manifest}))?;
        let before = open.project.clone();
        let missing = open.offline.missing.clone();
        let view = open.project_view();
        let picture = open.try_render_program_at(0.25, Tick::ZERO)?.over_black_rgba8();
        assert_eq!(review::impact(&open, &f.review.revision, &f.review)?, *result);
        assert!(Arc::ptr_eq(&before, &open.project));
        assert_eq!(open.offline.missing, missing);
        assert_eq!(open.project_view(), view);
        assert_eq!(open.try_render_program_at(0.25, Tick::ZERO)?.over_black_rgba8(), picture);
        assert!(!open.history.can_undo() && !open.is_dirty() && open.dirty_folder_bins().is_empty());
        compare_editor(&f.manifest, &f.review, result)?;
        assert_eq!(fs::read(&f.manifest)?, manifest);
    }
    Ok(())
}

#[test]
fn interrupted_or_incomplete_checks_cannot_return_a_source_only_prediction() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = cut(temp.path(), Case::Dependent)?;
    let cache = temp.path().join("cache");
    let original = fs::read(&f.review.path)?;
    let replaced = check(&f.manifest, &cache, &f.review, || {
        fs::write(&f.review.path, b"replaced")?;
        Ok(())
    });
    assert!(replaced.unwrap_err().to_string().contains("candidate changed"));
    fs::write(&f.review.path, original)?;
    let manifest: Value = serde_json::from_slice(&fs::read(&f.manifest)?)?;
    let records: Vec<(ItemId, String)> = serde_json::from_value(manifest["items"].clone())?;
    let hash = &records.iter().find(|(id, _)| *id == f.script).unwrap().1;
    let script = f.manifest.with_file_name(hash);
    let bytes = fs::read(&script)?;
    fs::remove_file(&script)?;
    assert!(check(&f.manifest, &cache, &f.review, || Ok(())).unwrap_err().to_string().contains("could not read item"));
    fs::write(script, bytes)?;
    let saved = check(&f.manifest, &cache, &f.review, || {
        let mut editor = Session::local_folder_editor();
        editor.execute("file.open", json!({"path":f.manifest}))?;
        editor.execute("clip.rename", json!({"item":f.master,"name":"Today's master"}))?;
        editor.execute("file.save", json!({}))?;
        Ok(())
    });
    assert!(saved.unwrap_err().to_string().contains("project changed during impact"));
    assert!(check(&f.manifest, &cache, &f.review, || Ok(())).unwrap_err().to_string().contains("project changed"));
    Ok(())
}
