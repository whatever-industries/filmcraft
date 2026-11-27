use super::Result;
use crate::native::preview::tests::fixture;
use filmcraft_engine::Session;
use filmcraft_engine::relink::review::Review;
use filmcraft_format::{atomic_write_new, encode_with_view, project_folder};
use filmcraft_project::ItemId;
use filmcraft_project::{BinEntry, ItemKind, Label, ParamValue};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};
use std::path::Path;
use std::{fs, path::PathBuf, sync::Arc};

#[derive(Clone, Copy)]
pub(super) enum Case {
    Plain,
    Dependent,
    Marked,
}

pub(super) struct Cut {
    pub(super) manifest: PathBuf,
    pub(super) review: Review,
    pub(super) nested: ItemId,
    pub(super) other: ItemId,
    pub(super) master: ItemId,
    pub(super) script: ItemId,
}

pub(super) fn cut(dir: &Path, case: Case) -> Result<Cut> {
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
    p.settings.title_safe = (13.0, 17.0);
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
    let mut document: Value = serde_json::from_slice(&encode_with_view(&s.project, Some(&s.project_view()), false))?;
    document["project"]["items"][source.0.to_string()]["extraSourceField"] = json!({"future":true});
    atomic_write_new(&dir_project, &serde_json::to_vec(&document)?)?;
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
