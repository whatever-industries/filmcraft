//! Original nested sequence fixture using FilmCraft's synthetic generator and stream metadata.
use super::super::verify;
use super::*;
use filmcraft_format::{decode, encode_with_view, project_folder};
use filmcraft_media::{DemoScene, Generator};
use filmcraft_project::{MediaRef, Project, ProjectView, SequenceSettings, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use std::{collections::BTreeSet, fs, path::PathBuf};

pub(super) struct Fixture {
    pub(super) path: PathBuf,
    pub(super) master: ItemId,
    pub(super) nested: ItemId,
    pub(super) camera: ItemId,
    pub(super) deferred: ItemId,
    pub(super) other_camera: ItemId,
    pub(super) other: BinId,
    pub(super) cameras: BinId,
}

pub(super) fn fixture(temp: &Path) -> Result<Fixture> {
    let seed = decode(include_bytes!("../../../format/tests/fixtures/v1-edit.fcproj"))?.project;
    let mut p = Project::new("Nested cut");
    let [masters, cuts, cameras, other] = ["Masters", "Cuts", "Cameras", "Other"].map(|name| p.add_bin(name, None));
    let seed_item = seed.item(ItemId(5)).unwrap();
    let camera = p.add_item("Camera", seed_item.label, seed_item.kind.clone(), Some(cameras));
    let media = p.item_mut(camera).unwrap().as_media_mut().unwrap();
    media.media = MediaRef::Generator(Generator::Demo(DemoScene::OceanSunset));
    let video = media.info.video.as_mut().unwrap();
    video.width = 128;
    video.height = 72;
    let kind = p.item(camera).unwrap().kind.clone();
    let other_camera = p.add_item("Second camera", seed_item.label, kind, Some(other));
    p.item_mut(other_camera).unwrap().as_media_mut().unwrap().media = MediaRef::Generator(Generator::Demo(DemoScene::CityNight));
    let settings = SequenceSettings { width: 128, height: 72, ..Default::default() };
    let nested = p.new_sequence("Cut", settings.clone(), 1, 1, Some(cuts));
    let master = p.new_sequence("Master", settings.clone(), 1, 1, Some(masters));
    let deferred = p.new_sequence("Other cut", settings, 1, 1, Some(other));
    for (sequence, source) in [(nested, camera), (master, nested), (deferred, other_camera)] {
        let rate = p.sequence(sequence).unwrap().settings.frame_rate;
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let item = p.make_track_item(source, kind, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
            let seq = p.sequence_mut(sequence).unwrap();
            if kind == TrackKind::Video { seq.video_tracks[0].items.push(item) } else { seq.audio_tracks[0].items.push(item) }
        }
    }
    let mut view = ProjectView { active_sequence: Some(master), open_sequences: vec![master, deferred], ..Default::default() };
    for (id, frame) in [(master, 7), (deferred, 14)] {
        view.playheads.insert(id, p.sequence(id).unwrap().settings.frame_rate.tick_of(frame));
    }
    let standard = temp.join("source.fcproj");
    atomic_write_new(&standard, &encode_with_view(&p, Some(&view), false))?;
    let dir = temp.join("folder");
    project_folder::pack(&standard, &dir, atomic_write_new)?;
    let path = dir.join(project_folder::ROOT);
    Ok(Fixture { path, master, nested, camera, deferred, other_camera, other, cameras })
}

#[test]
fn closed_bin_dependencies_play_before_navigation_and_unrelated_damage_is_deferred() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let Fixture { path, master, nested, camera, deferred, other, cameras, .. } = fixture(temp.path())?;
    let dir = path.parent().unwrap();
    let result = verify(&path, other)?;
    assert_eq!(result["pictureVaries"], true);
    assert_eq!(result["nonSilent"], true);
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let hashes: Vec<(ItemId, String)> = serde_json::from_value(manifest["items"].clone())?;
    let hash = hashes.into_iter().find(|(id, _)| *id == deferred).unwrap().1;
    fs::remove_file(dir.join(hash))?;
    let mut reader = Reader::open(&path)?;
    reader.startup(false)?;
    assert_eq!(reader.items.keys().copied().collect::<BTreeSet<_>>(), [master, nested, camera].into());
    let before = reader.bytes;
    reader.activate(cameras)?;
    assert_eq!(reader.bytes, before);
    assert!(reader.activate(other).is_err());
    assert!(reader.startup(true).is_err());
    Ok(())
}

#[test]
fn preview_lists_deferred_cut_and_retries_a_missing_nested_source_without_losing_the_active_frame() -> Result<()> {
    use egui::accesskit::Role;
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let catalog = temp.path().join("names.json");
    index(&f.path, &catalog)?;
    let original_manifest = fs::read(&f.path)?;
    let manifest: serde_json::Value = serde_json::from_slice(&original_manifest)?;
    let hashes: Vec<(ItemId, String)> = serde_json::from_value(manifest["items"].clone())?;
    let source_path = f.path.parent().unwrap().join(&hashes.iter().find(|(id, _)| *id == f.other_camera).unwrap().1);
    let source = fs::read(&source_path)?;
    let mut reference = Session::local_folder_editor();
    reference.execute("file.open", json!({"path":f.path}))?;
    let picture = |s: &Session| {
        let frame = s.render_program(0.25).unwrap();
        egui::ColorImage::from_rgba_unmultiplied([frame.w, frame.h], &frame.over_black_rgba8())
    };
    let sound = |s: &Session| {
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), 14000, 240, &provider).channels
    };
    let first = picture(&reference);
    reference.execute("sequence.open", json!({"item":f.deferred}))?;
    let second = picture(&reference);
    let second_sound = sound(&reference);
    assert_ne!(second, first);
    assert!(second_sound.iter().flatten().any(|v| v.abs() > 0.00001));
    fs::remove_file(&source_path)?;
    let view = Preview::open(&f.path, Some(&catalog))?;
    assert_eq!(view.image, first);
    let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(PathBuf::from);
    let builder = Harness::builder().with_size(egui::vec2(800.0, 600.0));
    let builder = if shots.is_some() { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, v: &mut Preview| v.show(ui), view);
    let resident = h.state().session.project.items.len();
    let bytes = h.state().reader.as_ref().unwrap().bytes;
    h.get_by_role_and_label(Role::Button, "Other").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Other cut");
    h.get_by_label("Program Monitor");
    assert_eq!(h.state().session.project.items.len(), resident);
    assert_eq!(h.state().reader.as_ref().unwrap().bytes, bytes);
    if let Some(dir) = shots {
        fs::create_dir_all(&dir)?;
        h.render()?.save(dir.join("read-only-bin-preview.png"))?;
    }
    h.get_by_role_and_label(Role::Tab, "Other cut").focus();
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().error.is_some());
    assert_eq!(h.state().view.active_sequence, Some(f.master));
    assert_eq!(h.state().image, first);
    assert_eq!(h.state().session.project.items.len(), resident);
    fs::write(&source_path, source)?;
    h.get_by_role_and_label(Role::Button, "Other cut").click();
    h.run();
    assert!(h.state().error.is_none());
    assert_eq!(h.state().view.open_sequences, [f.master, f.deferred]);
    assert_eq!(h.state().view.active_sequence, Some(f.deferred));
    assert_eq!(h.state().session.playhead(), reference.playhead());
    assert_eq!(h.state().image, second);
    assert_eq!(sound(&h.state().session), second_sound);
    let bytes = h.state().reader.as_ref().unwrap().bytes;
    h.get_by_role_and_label(Role::Tab, "Master").click();
    h.run();
    assert_eq!(h.state().image, first);
    assert_eq!(h.state().reader.as_ref().unwrap().bytes, bytes);
    assert_eq!(fs::read(&f.path)?, original_manifest);
    let mut changed = original_manifest;
    changed.push(b' ');
    fs::write(&f.path, changed)?;
    assert!(Preview::open(&f.path, Some(&catalog)).err().unwrap().to_string().contains("name index"));
    Ok(())
}
