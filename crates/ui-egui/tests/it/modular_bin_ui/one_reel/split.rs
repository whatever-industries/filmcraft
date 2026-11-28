use super::*;

pub(super) fn all_tracks(r: &mut Run) {
    let before = r.reference.project.clone();
    let original = before.sequence(r.reel).unwrap();
    let at = r.reference.sequence_rate().tick_of(12);
    for s in [&mut r.reference, &mut r.warm] {
        let result = s.execute("sequence.addEditAllTracks", json!({"time":at})).unwrap();
        assert!(result["cuts"].as_u64().unwrap() >= 2);
    }
    let q = r.warm.project.sequence(r.reel).unwrap();
    assert_eq!(Some(q), r.reference.project.sequence(r.reel));
    assert_eq!(q.duration(), original.duration());
    for (track, old) in q.all_tracks().zip(original.all_tracks()) {
        if old.locked {
            assert_eq!(track, old);
        } else if let Some(clip) = old.item_at(at) {
            let right = track.item_at(at).unwrap();
            assert_eq!(right.start, at);
            assert_eq!(right.end(), clip.end());
            assert_eq!(right.item, clip.item);
            assert!(right.id.0 >= before.next_id);
        }
    }
    let right = q.video_tracks[0].item_at(at).unwrap();
    assert!(right.link.is_some());
    assert_eq!(right.link, q.audio_tracks[0].item_at(at).unwrap().link);
    assert_ne!(right.link, original.video_tracks[0].item_at(at).unwrap().link);
    assert_eq!(r.warm.project.next_id, r.reference.project.next_id);
    assert_eq!(r.warm.dirty_folder_bins(), [r.bin].into());
    r.unloaded();
}

#[test]
fn save_discard_and_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = Run::from_folder(tmp.path(), fixture(tmp.path(), true));
    let prior = manifest(&r.folder);
    let original = r.reference.project.clone();
    all_tracks(&mut r);
    r.check_undo_redo_discard(&original, &prior);
    let at = r.warm.sequence_rate().tick_of(12);
    r.warm.execute("sequence.addEditAllTracks", json!({"time":at})).unwrap();
    r.warm.execute("file.save", json!({})).unwrap();
    r.check_saved_and_reopen(tmp.path(), &prior);
    assert_eq!(r.warm.execute("sequence.addEditAllTracks", json!({"time":at})).unwrap()["cuts"], 0);
    assert_eq!(r.warm.project.next_id, r.reference.project.next_id);
    assert!(!r.warm.is_dirty());
    another_split(&mut r.warm, r.reference.project.next_id);
    r.unloaded();
}

#[test]
fn add_edit_shortcut_and_razor_click_split_linked_media() {
    for razor in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let r = Run::from_folder(tmp.path(), fixture(tmp.path(), true));
        let original = r.reference.project.sequence(r.reel).unwrap();
        let clip = &r.args["clip"];
        let mut d = Driver::open(r.warm, support::with_shots(support::builder()));
        d.ok("ui.set", json!({"focused":"Timeline"}));
        if razor {
            d.exec("tool.razor", json!({}));
            let [x, y, w, h] = d.expect_rect(&format!("timeline.clip.{clip}"));
            let pos = egui::pos2((x + w / 2.0) as f32, (y + h / 2.0) as f32);
            support::pointer_drag(&mut d, pos, pos, 1);
        } else {
            d.exec("timeline.select", json!({"clips":[clip],"linked":true}));
            d.exec("playhead.set", json!({"frame":12}));
            d.ok("ui.key", json!({"key":"Cmd+K"}));
        }
        d.frames(4);
        let s = &d.app().session;
        let q = s.project.sequence(r.reel).unwrap();
        for (track, old) in [(&q.video_tracks[0], &original.video_tracks[0]), (&q.audio_tracks[0], &original.audio_tracks[0])] {
            assert_eq!(track.items.len(), old.items.len() + 1);
            assert_eq!(track.items[0].end(), track.items[1].start);
            assert_eq!(track.items[0].source_out(), track.items[1].source_in);
        }
        assert_eq!(q.video_tracks[0].items[1].link, q.audio_tracks[0].items[1].link);
        assert_eq!(q.audio_tracks[1], original.audio_tracks[1]);
        assert_eq!(s.dirty_folder_bins(), [r.bin].into());
        assert!(r.deferred.iter().all(|id| s.project.item(*id).is_none()));
    }
}

#[test]
fn caption_split_and_track_targeting_are_scoped_to_the_active_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = fixture(tmp.path(), true);
    let mut setup = open(&folder);
    let caption = setup.execute("captions.add", json!({"time":0,"durationSeconds":2,"text":"First line"})).unwrap()["caption"].clone();
    setup.execute("file.save", json!({})).unwrap();
    let mut r = Run::from_folder(tmp.path(), folder);
    let before = r.reference.project.clone();
    let at = r.reference.sequence_rate().tick_of(12);
    for s in [&mut r.reference, &mut r.warm] {
        s.execute("timeline.setTargeting", json!({"track":"V1","targeted":false})).unwrap();
        s.execute("sequence.addEdit", json!({"time":at})).unwrap();
        s.execute("captions.split", json!({"caption":caption, "time":at})).unwrap();
    }
    let q = r.warm.project.sequence(r.reel).unwrap();
    assert_eq!(q.video_tracks[0], before.sequence(r.reel).unwrap().video_tracks[0]);
    assert_eq!(q.audio_tracks[0].item_at(at).unwrap().start, at);
    assert_eq!(q.caption_tracks[0].captions.len(), 2);
    assert_eq!(Some(q), r.reference.project.sequence(r.reel));
    r.warm.execute("project.saveBin", json!({"bin":r.bin})).unwrap();
    assert_eq!(open(&r.folder).project, r.reference.project);
    r.unloaded();
}
