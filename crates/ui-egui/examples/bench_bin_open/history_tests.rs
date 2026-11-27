//! Compare every published inventory to the writable editor after actual bin decisions.
use super::tests::{Fixture, fixture};
use super::*;
use std::fs;

fn open(path: &Path) -> Result<Session> {
    let mut s = Session::local_folder_editor();
    s.execute("file.open", json!({"path":path}))?;
    Ok(s)
}

fn refresh_and_compare(f: &Fixture, old: &Path, out: &Path) -> Result<Preview> {
    refresh(&f.path, Some(old), out)?;
    let disk = open(&f.path)?;
    let view = Preview::open(&f.path, Some(out))?;
    assert_eq!(view.session.project.root, disk.project.root);
    let expected: BTreeMap<_, _> = disk.project.items.iter().map(|(&id, it)| (id, (it.name.as_str(), it.as_sequence().is_some()))).collect();
    assert_eq!(view.labels.iter().map(|(&id, l)| (id, (l.name.as_str(), l.sequence))).collect::<BTreeMap<_, _>>(), expected);
    assert_eq!(view.view.open_sequences, disk.state.open_sequences);
    Ok(view)
}

fn picture(s: &Session) -> Vec<u8> {
    s.render_program(0.25).unwrap().over_black_rgba8()
}

#[test]
fn published_index_tracks_moves_discard_and_restore_without_retargeting_the_master() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let name = |stage: &str| temp.path().join(format!("{stage}.json"));
    let first = name("initial");
    index(&f.path, &first)?;
    let mut s = open(&f.path)?;
    let initial = s.folder_revision().unwrap().to_owned();
    let cuts = s.project.root.parent_of(f.nested).unwrap();
    let rename = |s: &mut Session, n: &str| s.execute("clip.rename", json!({"item":f.nested,"name":n}));
    rename(&mut s, "Unsaved title")?;
    s.execute("project.discardBin", json!({"bin":cuts}))?;
    assert_eq!(s.project.item(f.nested).unwrap().name, "Cut");
    let discarded = name("discarded");
    refresh_and_compare(&f, &first, &discarded)?;
    assert_eq!(fs::read(&first)?, fs::read(&discarded)?);
    rename(&mut s, "Assembly")?;
    s.execute("project.renameBin", json!({"bin":cuts,"name":"Earlier cuts"}))?;
    s.execute("project.saveBin", json!({"bin":cuts}))?;
    let mut current = name("renamed");
    refresh_and_compare(&f, &discarded, &current)?;
    let mut versions = vec![picture(&open(&f.path)?)];
    for (dest, source) in [(f.other, f.other_camera), (f.cameras, f.camera)] {
        s.execute("project.moveToBin", json!({"items":[f.nested],"bin":dest}))?;
        s.edit("Change nested source", |p, _| {
            for track in p.sequence_mut(f.nested).unwrap().all_tracks_mut() {
                track.items[0].item = source;
                if dest == f.cameras {
                    track.items[0].source_in = filmcraft_time::Tick(filmcraft_time::TICKS_PER_SECOND / 2);
                }
            }
            Ok(())
        })?;
        s.execute("project.saveBin", json!({"bin":dest}))?;
        let updated = name(&format!("saved-{}", dest.0));
        refresh_and_compare(&f, &current, &updated)?;
        current = updated;
        let master = picture(&open(&f.path)?);
        assert_ne!(&master, versions.last().unwrap());
        versions.push(master);
    }
    let disk = open(&f.path)?;
    for order in [
        [cuts, f.other, f.cameras],
        [cuts, f.cameras, f.other],
        [f.other, cuts, f.cameras],
        [f.other, f.cameras, cuts],
        [f.cameras, cuts, f.other],
        [f.cameras, f.other, cuts],
    ] {
        let mut view = Preview::open(&f.path, Some(&current))?;
        for bin in order {
            view.bin = bin;
            let cut = super::super::direct(view.session.project.root.find_bin(bin).unwrap()).find(|id| view.labels[id].name.starts_with("Assembly")).unwrap();
            assert_eq!(cut == f.nested, bin == f.cameras);
            view.select(cut)?;
            let mut reference = open(&f.path)?;
            reference.execute("sequence.open", json!({"item":cut}))?;
            assert_eq!(picture(&view.session), picture(&reference));
            view.select(f.master)?;
            assert_eq!(picture(&view.session), picture(&disk));
        }
    }
    let mut s = open(&f.path)?;
    let restored = s.execute("project.restoreSequenceCopy", json!({"revision":initial,"sequence":f.nested}))?;
    let copy = ItemId(restored["item"].as_u64().unwrap());
    s.execute("file.save", json!({}))?;
    let mut view = refresh_and_compare(&f, &current, &name("restored"))?;
    assert_ne!(copy, f.nested);
    assert_eq!(view.session.project.root.parent_of(copy), Some(f.cameras));
    view.select(f.master)?;
    assert_eq!(picture(&view.session), versions.pop().unwrap());
    view.select(copy)?;
    assert_eq!(view.session.project.sequence(copy).unwrap().video_tracks[0].items[0].source_in, filmcraft_time::Tick::ZERO);
    assert_ne!(view.session.project.sequence(f.nested).unwrap().video_tracks[0].items[0].source_in, filmcraft_time::Tick::ZERO);
    assert_eq!(view.session.project.sequence(copy).unwrap().video_tracks[0].items[0].item, f.camera);
    Ok(())
}

#[test]
fn failed_refresh_leaves_prior_index_and_retry_matches_full_rebuild() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let old = temp.path().join("old.json");
    let next = temp.path().join("next.json");
    index(&f.path, &old)?;
    let old_bytes = fs::read(&old)?;
    let mut s = open(&f.path)?;
    s.execute("clip.rename", json!({"item":f.nested,"name":"Published cut"}))?;
    s.execute("file.save", json!({}))?;
    let m: serde_json::Value = serde_json::from_slice(&fs::read(&f.path)?)?;
    let hashes: Vec<(ItemId, String)> = serde_json::from_value(m["items"].clone())?;
    let payload = f.path.parent().unwrap().join(&hashes.iter().find(|v| v.0 == f.nested).unwrap().1);
    let missing = temp.path().join("outside");
    fs::rename(&payload, &missing)?;
    assert!(refresh(&f.path, Some(&old), &next).is_err());
    assert!(!next.exists());
    assert_eq!(fs::read(&old)?, old_bytes);
    assert!(Preview::open(&f.path, Some(&old)).is_err());
    fs::rename(&missing, payload)?;
    // A refresh can reuse the label of an unchanged deferred item. Selecting that cut
    // still has to verify its payload, and a full rebuild needs the payload as well.
    let deferred = f.path.parent().unwrap().join(&hashes.iter().find(|v| v.0 == f.deferred).unwrap().1);
    fs::rename(&deferred, &missing)?;
    refresh(&f.path, Some(&old), &next)?;
    let mut view = Preview::open(&f.path, Some(&next))?;
    assert!(view.select(f.deferred).is_err());
    assert!(index(&f.path, &temp.path().join("incomplete.json")).is_err());
    fs::rename(&missing, deferred)?;
    let rebuilt = temp.path().join("rebuilt.json");
    index(&f.path, &rebuilt)?;
    assert_eq!(fs::read(&rebuilt)?, fs::read(&next)?);
    let mut corrupt: serde_json::Value = serde_json::from_slice(&old_bytes)?;
    corrupt["labels"][f.nested.0.to_string()]["name"] = "Wrong".into();
    fs::write(&old, serde_json::to_vec(&corrupt)?)?;
    assert!(refresh(&f.path, Some(&old), &temp.path().join("bad.json")).is_err());
    Ok(())
}

#[test]
fn saved_removal_and_discarded_arrival_leave_the_source_playable_without_a_bin_entry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = fixture(temp.path())?;
    let old = temp.path().join("initial.json");
    index(&f.path, &old)?;
    let mut s = open(&f.path)?;
    let cuts = s.project.root.parent_of(f.nested).unwrap();
    let master = picture(&s);
    s.execute("project.moveToBin", json!({"items":[f.nested],"bin":f.other}))?;
    s.execute("project.saveBin", json!({"bin":cuts}))?;
    s.execute("project.discardBin", json!({"bin":f.other}))?;
    let view = refresh_and_compare(&f, &old, &temp.path().join("published.json"))?;
    assert_eq!(view.session.project.root.parent_of(f.nested), None);
    assert_eq!(picture(&view.session), master);
    assert!(view.session.project.sequence(f.nested).is_some());
    Ok(())
}
