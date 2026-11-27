//! Original synthetic camera sources, nested cuts and masters built for the move tests.
use super::*;
use filmcraft_media::{DemoScene, Generator};
use filmcraft_project::{Camera, MediaRef, MulticamAudio, MulticamSel, MulticamSource, Project, SequenceSettings};

struct Feature {
    path: PathBuf,
    bins: [BinId; 3],
    cut: ItemId,
    group: ItemId,
    masters: [ItemId; 2],
}

fn add_tracks(p: &mut Project, sequence: ItemId, index: usize, source: ItemId) {
    let rate = p.sequence(sequence).unwrap().settings.frame_rate;
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let clip = p.make_track_item(source, kind, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(48)), rate).unwrap();
        let q = p.sequence_mut(sequence).unwrap();
        match kind {
            TrackKind::Video => q.video_tracks[index].items.push(clip),
            _ => q.audio_tracks[index].items.push(clip),
        }
    }
}

impl Feature {
    fn create(dir: &Path) -> Self {
        // Reuse the repository's own synthetic-media description for stream metadata.
        let seed = filmcraft_format::decode(include_bytes!("../../../../format/tests/fixtures/v1-edit.fcproj")).unwrap();
        let mut p = Project::new("Feature");
        let bins = ["Bin A", "Bin B", "Bin C"].map(|n| p.add_bin(n, None));
        let settings = SequenceSettings { width: 128, height: 72, ..Default::default() };
        let group = p.new_sequence("Camera group", settings.clone(), 3, 3, None);
        let mut cameras = Vec::new();
        for (index, scene) in [DemoScene::OceanSunset, DemoScene::CityNight, DemoScene::Dunes].into_iter().enumerate() {
            let item = seed.project.item(ItemId(5)).unwrap();
            let id = p.add_item(scene.file_name(), item.label, item.kind.clone(), None);
            let media = p.item_mut(id).unwrap().as_media_mut().unwrap();
            media.media = MediaRef::Generator(Generator::Demo(scene));
            let info = media.info.video.as_mut().unwrap();
            info.width = 128;
            info.height = 72;
            add_tracks(&mut p, group, index, id);
            let q = p.sequence(group).unwrap();
            cameras.push(Camera {
                name: scene.file_name().into(),
                video_track: Some(q.video_tracks[index].id),
                audio_tracks: vec![q.audio_tracks[index].id],
                enabled: true,
                source: Some(id),
            });
        }
        p.sequence_mut(group).unwrap().multicam = Some(MulticamSource { cameras, audio: MulticamAudio::SwitchAudio, ..Default::default() });
        let cut = p.new_sequence("Sequence 1", settings.clone(), 1, 1, Some(bins[0]));
        add_tracks(&mut p, cut, 0, group);
        for track in p.sequence_mut(cut).unwrap().all_tracks_mut() {
            track.items[0].multicam = Some(MulticamSel { enabled: true, angle: 0 });
        }
        let masters = ["Master 1", "Master 2"].map(|name| {
            let id = p.new_sequence(name, settings.clone(), 1, 1, None);
            add_tracks(&mut p, id, 0, cut);
            id
        });
        let standard = dir.join("feature.fcproj");
        fs::write(&standard, filmcraft_format::encode(&p, false)).unwrap();
        let folder = dir.join("folder");
        filmcraft_format::project_folder::pack(&standard, &folder, filmcraft_format::atomic_write_new).unwrap();
        Self { path: folder.join("project.fcmod"), bins, cut, group, masters }
    }

    fn angle(&self, s: &mut Session, angle: u32) {
        s.edit("Change angle", |p, _| {
            for t in p.sequence_mut(self.cut).unwrap().all_tracks_mut() {
                t.items[0].multicam.as_mut().unwrap().angle = angle;
            }
            Ok(())
        })
        .unwrap();
    }

    fn recall(&self, s: &mut Session) {
        let tick = s.sequence_rate().tick_of(7);
        s.set_playhead(tick);
        s.execute("sequence.matchFrame", json!({})).unwrap();
        assert_eq!(s.state.source_item, Some(self.cut));
        assert_eq!(s.state.source_playhead, tick);
        assert_eq!(s.project.sequence(self.group).unwrap().multicam.as_ref().unwrap().cameras.len(), 3);
    }
}

#[derive(PartialEq)]
struct Sample {
    picture: Vec<u8>,
    sound: Vec<Vec<f32>>,
    png: Vec<u8>,
    wav: Vec<u8>,
}
fn sample(s: &mut Session, master: ItemId, dir: &Path) -> Sample {
    s.execute("sequence.open", json!({"item":master})).unwrap();
    s.set_playhead(s.sequence_rate().tick_of(7));
    let picture = s.render_program(1.0).unwrap().over_black_rgba8();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let sound = filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), 14000, 240, &provider).channels;
    let png = dir.join("sample.png");
    let wav = dir.join("sample.wav");
    s.execute("file.exportFrame", json!({"path":png,"monitor":"program"})).unwrap();
    s.execute("file.exportMedia", json!({"path":wav,"format":"wav","range":"custom","startSeconds":0,"endSeconds":0.25,"wait":true})).unwrap();
    let out = Sample { picture, sound, png: fs::read(&png).unwrap(), wav: fs::read(&wav).unwrap() };
    fs::remove_file(png).unwrap();
    fs::remove_file(wav).unwrap();
    out
}

#[test]
fn last_saved_arrival_wins_in_all_bin_orders_and_after_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let f = Feature::create(dir.path());
    let mut s = open(&f.path);
    let mut versions = vec![sample(&mut s, f.masters[0], dir.path())];
    let mut direct = vec![sample(&mut s, f.cut, dir.path())];
    for index in [1, 2] {
        s.execute("project.moveToBin", json!({"items":[f.cut],"bin":f.bins[index]})).unwrap();
        f.angle(&mut s, index as u32);
        let reference = sample(&mut s, f.masters[0], dir.path());
        for prior in &versions {
            assert!(prior.picture != reference.picture && prior.sound != reference.sound && prior.png != reference.png && prior.wav != reference.wav);
        }
        versions.push(reference);
        direct.push(sample(&mut s, f.cut, dir.path()));
        s.execute("project.saveBin", json!({"bin":f.bins[index]})).unwrap();
        let mut disk = open(&f.path);
        for master in f.masters {
            assert!(sample(&mut disk, master, dir.path()) == versions[index]);
            f.recall(&mut disk);
        }
    }
    let disk = open(&f.path);
    let copies: Vec<_> = f
        .bins
        .iter()
        .map(|bin| {
            disk.project
                .root
                .find_bin(*bin)
                .unwrap()
                .children
                .iter()
                .find_map(|e| match e {
                    filmcraft_project::BinEntry::Item(i) => Some(*i),
                    _ => None,
                })
                .unwrap()
        })
        .collect();
    assert_ne!(copies[0], copies[1]);
    assert_ne!(copies[0], f.cut);
    assert_ne!(copies[1], f.cut);
    assert_eq!(copies[2], f.cut);
    let mut late = open(&f.path);
    late.execute("project.renameBin", json!({"bin":f.bins[0],"name":"Renamed bin"})).unwrap();
    late.execute("project.saveBin", json!({"bin":f.bins[0]})).unwrap();
    let standard = dir.path().join("checkout.fcproj");
    filmcraft_format::project_folder::checkout(f.path.parent().unwrap(), &standard).unwrap();
    let mut checkout = Session::default();
    checkout.execute("file.open", json!({"path":standard})).unwrap();
    for (index, id) in copies.iter().enumerate() {
        assert_eq!(checkout.project.root.parent_of(*id), Some(f.bins[index]));
        assert!(sample(&mut checkout, *id, dir.path()) == direct[index]);
    }
    for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        let mut d = Driver::open(open(&f.path), support::builder());
        for index in std::iter::once(None).chain(order.map(Some)) {
            if let Some(i) = index {
                invoke(&mut d, "openBin", json!({"bin":f.bins[i],"how":"tab"}));
            }
            for master in f.masters {
                assert!(sample(&mut d.app().session, master, dir.path()) == versions[2]);
                f.recall(&mut d.app().session);
            }
        }
    }
}

#[test]
fn master_recalls_last_saved_source_after_source_save_and_destination_discard() {
    let dir = tempfile::tempdir().unwrap();
    let f = Feature::create(dir.path());
    let mut d = Driver::open(open(&f.path), support::with_shots(support::builder()));
    for bin in &f.bins[..2] {
        invoke(&mut d, "openBin", json!({"bin":bin,"how":"tab"}));
    }
    let original = sample(&mut d.app().session, f.masters[0], dir.path());
    f.angle(&mut d.app().session, 1);
    let departure = sample(&mut d.app().session, f.cut, dir.path());
    d.exec("project.moveToBin", json!({"items":[f.cut],"bin":f.bins[1]}));
    f.angle(&mut d.app().session, 2);
    for (bin, decision) in [(f.bins[0], "save"), (f.bins[1], "discard")] {
        invoke(&mut d, "closeBinTab", json!({"bin":bin}));
        if bin == f.bins[0] {
            assert!(d.harness.query_by_label_contains("move backup in project history").is_some());
            let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::Dialog, "Save changes to Bin A?");
            support::save_shot(&mut d, 0, "sequence-move-backup.png");
        }
        d.click(&format!("project.closeBin.{decision}"));
    }
    let mut disk = open(&f.path);
    assert_eq!(disk.project.root.parent_of(f.cut), None);
    for master in f.masters {
        assert!(sample(&mut disk, master, dir.path()) == original);
        f.recall(&mut disk);
    }
    let mut hash = None;
    let folder = f.path.parent().unwrap();
    filmcraft_format::project_folder::move_backups(folder, |_, b| {
        hash = Some(b.snapshot.clone());
        Ok(())
    })
    .unwrap();
    let hash = hash.unwrap();
    let (copy, _) = filmcraft_format::project_folder::restore_copy(folder, disk.folder_revision().unwrap(), &hash, f.cut.0).unwrap();
    let mut restored = open(&f.path);
    assert!(sample(&mut restored, ItemId(copy), dir.path()) == departure);
    assert!(sample(&mut restored, f.masters[0], dir.path()) == original);
    restored
        .edit("Edit required camera group", |p, _| {
            p.sequence_mut(f.group).unwrap().start_timecode = 15;
            Ok(())
        })
        .unwrap();
    restored.execute("file.save", json!({})).unwrap();
    let revision = restored.folder_revision().unwrap();
    assert!(filmcraft_format::project_folder::restore_copy(folder, revision, &hash, f.cut.0).unwrap_err().to_string().contains("rescue"));
    let rescue = dir.path().join("rescue.fcproj");
    filmcraft_format::project_folder::rescue(folder, &hash, f.cut.0, &rescue).unwrap();
    let mut historical = Session::default();
    historical.execute("file.open", json!({"path":rescue})).unwrap();
    assert!(sample(&mut historical, f.cut, dir.path()) == departure);
    assert_eq!(open(&f.path).folder_revision(), Some(revision));
}

#[test]
fn kill_after_each_arrival_or_final_discard_observes_the_published_cut() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    const CHILD: &str = "FILMCRAFT_MOVE_CRASH_ROOT";
    const CASE: &str = "FILMCRAFT_MOVE_CRASH_DECISION";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let f = Feature::create(&root);
        let case = std::env::var(CASE).unwrap();
        let mut d = Driver::open(open(&f.path), support::builder());
        d.app().session.start_autosave(filmcraft_engine::autosave::AutosaveConfig::new(&root)).unwrap();
        d.exec("file.projectSettings.general", json!({"titleSafe":[12,14]}));
        for step in 1..=if case == "second" { 2 } else { 1 } {
            for bin in [f.bins[step - 1], f.bins[step]] {
                invoke(&mut d, "openBin", json!({"bin":bin}));
            }
            d.exec("project.moveToBin", json!({"items":[f.cut],"bin":f.bins[step]}));
            f.angle(&mut d.app().session, step as u32);
            if case == "discard" {
                invoke(&mut d, "closeBinTab", json!({"bin":f.bins[0]}));
                d.click("project.closeBin.save");
            }
            invoke(&mut d, "closeBinTab", json!({"bin":f.bins[step]}));
            d.click(if case == "discard" { "project.closeBin.discard" } else { "project.closeBin.save" });
            assert!(d.app().ui.project_panel.tabs.iter().all(|t| t.bin != f.bins[step].0));
        }
        println!("transfer acknowledged");
        std::thread::park();
        return;
    }
    for (case, angle, owner) in [("first", 1, Some("Bin B")), ("second", 2, Some("Bin C")), ("discard", 0, None)] {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "modular_bin_ui::transfer_playback::kill_after_each_arrival_or_final_discard_observes_the_published_cut", "--nocapture"])
            .env(CHILD, dir.path())
            .env(CASE, case)
            .env_remove("FILMCRAFT_UI_SHOTS")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let acknowledged = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|l| l.contains("transfer acknowledged"));
        if acknowledged {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        assert!(acknowledged, "{case}");
        let live = open(&dir.path().join("folder/project.fcmod"));
        let cut = live.project.sequences().find(|i| i.name == "Sequence 1").unwrap().id;
        let bin = live.project.root.parent_of(cut).and_then(|id| live.project.root.find_bin(id));
        assert_eq!(bin.map(|b| b.name.as_str()), owner);
        for master in live.project.sequences().filter(|i| i.name.starts_with("Master ")) {
            assert_eq!(master.as_sequence().unwrap().video_tracks[0].items[0].item, cut);
        }
        assert_eq!(live.project.sequence(cut).unwrap().video_tracks[0].items[0].multicam.unwrap().angle, angle);
        let mut draft = Session::local_folder_editor();
        draft.start_autosave(filmcraft_engine::autosave::AutosaveConfig::new(dir.path())).unwrap();
        draft.execute("file.recover", json!({})).unwrap();
        assert_eq!(draft.project.settings.title_safe, (12.0, 14.0));
        assert_eq!(draft.project.sequence(cut).unwrap().video_tracks[0].items[0].multicam.unwrap().angle, angle);
        draft.shutdown();
    }
}
