//! Original nested sequence fixture using FilmCraft's synthetic generator and stream metadata.
use super::*;
use filmcraft_media::{DemoScene, Generator};
use filmcraft_project::{MediaRef, ProjectView, SequenceSettings, TrackKind};
use filmcraft_time::{Tick, TimeRange};

#[test]
fn closed_bin_dependencies_play_before_navigation_and_unrelated_damage_is_deferred() -> Result<()> {
    let temp = tempfile::tempdir()?;
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
    let settings = SequenceSettings { width: 128, height: 72, ..Default::default() };
    let nested = p.new_sequence("Cut", settings.clone(), 1, 1, Some(cuts));
    let master = p.new_sequence("Master", settings.clone(), 1, 1, Some(masters));
    let deferred = p.new_sequence("Other cut", settings, 1, 1, Some(other));
    for (sequence, source) in [(nested, camera), (master, nested)] {
        let rate = p.sequence(sequence).unwrap().settings.frame_rate;
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let item = p.make_track_item(source, kind, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
            let seq = p.sequence_mut(sequence).unwrap();
            if kind == TrackKind::Video { seq.video_tracks[0].items.push(item) } else { seq.audio_tracks[0].items.push(item) }
        }
    }
    let view = ProjectView { active_sequence: Some(master), open_sequences: vec![master, deferred], ..Default::default() };
    let standard = temp.path().join("source.fcproj");
    atomic_write_new(&standard, &encode_with_view(&p, Some(&view), false))?;
    let dir = temp.path().join("folder");
    project_folder::pack(&standard, &dir, atomic_write_new)?;
    let path = dir.join(project_folder::ROOT);
    let result = verify(&path, other)?;
    assert_eq!(result["pictureVaries"], true);
    assert_eq!(result["nonSilent"], true);
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let hashes: Vec<(ItemId, String)> = serde_json::from_value(manifest["items"].clone())?;
    let hash = hashes.into_iter().find(|(id, _)| *id == deferred).unwrap().1;
    fs::remove_file(dir.join(hash))?;
    let mut reader = reader::Reader::open(&path)?;
    reader.startup(false)?;
    assert_eq!(reader.items.keys().copied().collect::<BTreeSet<_>>(), [master, nested, camera].into());
    let before = reader.bytes;
    reader.activate(cameras)?;
    assert_eq!(reader.bytes, before);
    assert!(reader.activate(other).is_err());
    assert!(reader.startup(true).is_err());
    Ok(())
}
