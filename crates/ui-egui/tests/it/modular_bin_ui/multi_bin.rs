//! Compare desktop bin decisions with independently edited standard projects.
use super::*;
use filmcraft_format::project_folder::{
    self, FolderDirectory,
    index::{Index, cache_directory},
};
use filmcraft_project::{ClipId, ProjectView};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::SystemTime,
};

mod acceptance;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Cut {
    bin: BinId,
    reel: ItemId,
    clip: ClipId,
    shorten: i64,
    boundary: Tick,
}

impl Cut {
    fn trim(&self, s: &mut Session) {
        s.execute("sequence.open", json!({"item":self.reel})).unwrap();
        let result = s.execute("timeline.trim", json!({"clip":self.clip,"edge":"out","deltaFrames":-self.shorten})).unwrap();
        assert_eq!(result["delta"], json!(s.sequence_rate().tick_of(-self.shorten)));
    }

    fn pictures(&self, s: &mut Session) -> [Vec<u8>; 2] {
        s.execute("sequence.open", json!({"item":self.reel})).unwrap();
        [self.boundary - s.sequence_rate().frame_duration(), self.boundary].map(|t| s.try_render_program_at(0.125, t).unwrap().over_black_rgba8())
    }
}

fn prepare_standard(path: &Path) -> [Cut; 2] {
    let mut s = if let Some(input) = std::env::var_os("FILMCRAFT_MULTI_BIN_PROJECT") {
        open(Path::new(&input))
    } else {
        let mut s = Session::default();
        let p = Arc::make_mut(&mut s.project);
        for name in ["First", "Second", "Unrelated"] {
            p.new_sequence(name, filmcraft_project::SequenceSettings { width: 160, height: 90, ..Default::default() }, 1, 0, None);
        }
        s
    };
    let reels: Vec<_> = s.project.sequences().map(|item| item.id).collect();
    let cuts = [(reels[0], "Cuts", "#ff3050", 5), (reels[1], "Selects", "#30dd60", 11)].map(|(reel, name, color, shorten)| {
        let matte = s.execute("file.newColorMatte", json!({"color":color,"width":160,"height":90,"seconds":4})).unwrap();
        let p = Arc::make_mut(&mut s.project);
        let bin = p.add_bin(name, None);
        assert_eq!(p.move_to_bin(&[reel], Some(bin)), Some(1));
        let seq = p.sequence(reel).unwrap();
        let rate = seq.settings.frame_rate;
        let start = rate.snap(seq.duration()) + rate.tick_of(1);
        let range = TimeRange::new(Tick::ZERO, rate.tick_of(36));
        let item = p.make_track_item(ItemId(matte["item"].as_u64().unwrap()), TrackKind::Video, start, range, rate).unwrap();
        let cut = Cut { bin, reel, clip: item.id, shorten, boundary: item.end() - rate.tick_of(shorten) };
        // Replace one placement on the large fixtures to keep their original counts.
        let track = &mut p.sequence_mut(reel).unwrap().video_tracks[0];
        track.items.pop();
        track.items.push(item);
        cut
    });
    Arc::make_mut(&mut s.project).resolve_placed_auto_points(|_| true, |_| true);
    let view = ProjectView { open_sequences: cuts.map(|c| c.reel).to_vec(), active_sequence: Some(cuts[0].reel), ..Default::default() };
    fs::write(path, filmcraft_format::encode_with_view(&s.project, Some(&view), false)).unwrap();
    cuts
}

fn resident(s: &Session) -> Vec<ItemId> {
    s.project.sequences().map(|item| item.id).collect()
}

fn driver(manifest: &Path, data: &Path) -> Driver {
    let (index, ..) = Index::build(&FolderDirectory::open(manifest).unwrap(), None).unwrap();
    fs::write(cache_directory(manifest, data).unwrap().join("index.json"), index.bytes().unwrap()).unwrap();
    let mut s = Session::local_folder_editor();
    s.enable_experimental_modular_relink(data.to_owned());
    s.execute("file.open", json!({"path":manifest})).unwrap();
    Driver::open(s, support::builder())
}

// Root publication consists of immutable digest-named files and the atomically replaced manifest.
struct Publication {
    head: Value,
    files: BTreeMap<String, (SystemTime, u64)>,
}

impl Publication {
    fn read(manifest: &Path) -> Self {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(manifest.parent().unwrap()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if name == project_folder::ROOT || (name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())) {
                let meta = entry.metadata().unwrap();
                files.insert(name, (meta.modified().unwrap(), meta.len()));
            }
        }
        Self { head: serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap(), files }
    }

    fn check(&self, before: &Self, expected: &[ItemId], label: &str) {
        let records = |head: &Value| serde_json::from_value::<Vec<(ItemId, String)>>(head["items"].clone()).unwrap();
        let old: BTreeMap<_, _> = records(&before.head).into_iter().collect();
        let new = records(&self.head);
        assert_eq!(new.len(), old.len());
        let changed: BTreeSet<_> = new.iter().filter(|(id, hash)| old.get(id).unwrap() != hash).map(|(id, _)| *id).collect();
        assert_eq!(changed, expected.iter().copied().collect(), "{label}");
        assert_eq!(self.head["bins"], before.head["bins"], "{label}: bin membership changed");
        let bytes: u64 = self.files.iter().filter(|(name, stamp)| before.files.get(*name) != Some(*stamp)).map(|(_, (_, len))| len).sum();
        assert_eq!(bytes == 0, expected.is_empty(), "{label}");
        assert_eq!(self.head == before.head, expected.is_empty(), "{label}");
        if !expected.is_empty() {
            let revision = |head: &Value| head["revision"]["number"].as_u64().unwrap_or(0);
            assert_eq!(revision(&self.head), revision(&before.head) + 1, "{label}: expected a single publication");
        }
        eprintln!("{label}: {bytes} publication bytes, changed items {changed:?}, envelope changed {}", self.head["envelope"] != before.head["envelope"]);
    }
}

fn compare_saved(manifest: &Path, expected: &Path, cuts: [Cut; 2]) {
    let mut reopened = open(manifest);
    let mut reference = open(expected);
    assert_eq!(reopened.project.settings, reference.project.settings);
    for cut in cuts {
        assert!(reopened.project.sequence(cut.reel) == reference.project.sequence(cut.reel), "reopened sequence {:?} differs", cut.reel);
        assert!(cut.pictures(&mut reopened) == cut.pictures(&mut reference), "frames differ around the trim in {:?}", cut.reel);
    }
}

fn assert_pending(d: &mut Driver, bins: &[BinId], settings: bool) {
    let status = d.exec("project.binStatus", json!({}));
    let dirty: BTreeSet<BinId> = serde_json::from_value(status["dirtyBins"].clone()).unwrap();
    assert_eq!(dirty, bins.iter().copied().collect());
    assert_eq!(status["projectSettingsDirty"], settings);
}

fn shortcut(d: &mut Driver) {
    d.ok("ui.set", json!({"focused":"Project"}));
    d.ok("ui.key", json!({"key":"Cmd+S"}));
    d.frames(3);
}

#[test]
fn two_bin_decisions_match_standard_project() {
    let tmp = tempfile::tempdir().unwrap();
    let standard = tmp.path().join("original.fcproj");
    let cuts = prepare_standard(&standard);
    let [first, second] = cuts;
    for case in ["save-close", "discard-close", "global"] {
        let folder = tmp.path().join(case);
        project_folder::pack(&standard, &folder, filmcraft_format::atomic_write_new).unwrap();
        let manifest = folder.join(project_folder::ROOT);
        let baseline = Publication::read(&manifest);
        let mut d = driver(&manifest, &tmp.path().join("index"));
        let mut reference = open(&standard);
        let placements: usize = reference
            .project
            .sequences()
            .filter_map(|item| item.as_sequence())
            .flat_map(|seq| seq.video_tracks.iter().chain(&seq.audio_tracks))
            .map(|track| track.items.len())
            .sum();
        eprintln!("{case}, {placements} placements; open: {:?}", resident(&d.app().session));
        let general = json!({"titleSafe":[12,14]});
        assert_ne!(reference.project.settings.title_safe, (12.0, 14.0));
        for cut in cuts {
            invoke(&mut d, "openBin", json!({"bin":cut.bin,"how":"tab"}));
            d.exec("sequence.open", json!({"item":cut.reel}));
            eprintln!("before trim {:?}: {:?}", cut.reel, resident(&d.app().session));
            cut.trim(&mut d.app().session);
            eprintln!("after trim {:?}: {:?}", cut.reel, resident(&d.app().session));
            let before = cut.pictures(&mut reference);
            cut.trim(&mut reference);
            let after = cut.pictures(&mut reference);
            assert!(before[0] == after[0] && before[1] != after[1], "fixture trim must be visible at its boundary");
            assert!(cut.pictures(&mut d.app().session) == after);
        }
        for cut in cuts {
            assert!(reference.project.sequence(cut.reel) == d.app().session.project.sequence(cut.reel));
        }
        d.exec("file.projectSettings.general", general.clone());
        assert_pending(&mut d, &[first.bin, second.bin], true);

        // Construct the independently saved reference using only the edits that should survive.
        let mut expected = open(&standard);
        first.trim(&mut expected);
        if case == "global" {
            second.trim(&mut expected);
        }
        if case != "save-close" {
            expected.execute("file.projectSettings.general", general.clone()).unwrap();
        }
        let expected_file = tmp.path().join(format!("{case}.fcproj"));
        expected.execute("file.saveAs", json!({"path":expected_file})).unwrap();

        if case != "global" {
            let (close, keep, button) = if case == "save-close" { (first, second, "save") } else { (second, first, "discard") };
            invoke(&mut d, "closeBinTab", json!({"bin":close.bin}));
            d.click(&format!("project.closeBin.{button}"));
            assert_pending(&mut d, &[keep.bin], true);
            let app = d.app();
            assert_eq!(app.session.project.settings.title_safe, (12.0, 14.0));
            assert!(app.session.project.sequence(keep.reel) == reference.project.sequence(keep.reel));
            assert!(app.ui.project_panel.tabs.iter().any(|t| t.bin == keep.bin.0));
            assert!(!app.ui.project_panel.tabs.iter().any(|t| t.bin == close.bin.0));
            assert!(app.session.state.open_sequences.contains(&keep.reel));
            assert!(!app.session.state.open_sequences.contains(&close.reel));
            eprintln!("after close: {:?}", resident(&app.session));
            if case == "discard-close" {
                Publication::read(&manifest).check(&baseline, &[], "Discard");
                compare_saved(&manifest, &standard, cuts);
            }
        }
        if case != "save-close" {
            invoke(&mut d, "openBin", json!({"bin":first.bin,"how":"tab"}));
            let panel = &d.app().ui.project_panel;
            assert_eq!(panel.tabs[panel.active_tab.unwrap()].bin, first.bin.0);
            shortcut(&mut d);
            assert_pending(&mut d, &[], false);
            eprintln!("after global Save: {:?}", resident(&d.app().session));
        }
        let saved = Publication::read(&manifest);
        let changed = if case == "global" { vec![first.reel, second.reel] } else { vec![first.reel] };
        saved.check(&baseline, &changed, case);
        compare_saved(&manifest, &expected_file, cuts);
        if case == "global" {
            shortcut(&mut d);
            Publication::read(&manifest).check(&saved, &[], "Repeated Save");
        }
    }
}
