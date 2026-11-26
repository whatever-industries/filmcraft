use super::*;

#[test]
fn milestone_pins_survive_saves_and_travel_with_an_independent_backup() {
    use converter::{backup::published_history, pins};
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("production");
    let backup = tmp.path().join("backup");
    let edit = tmp.path().join("edit.fcproj");
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let initial = checkout(&folder, &tmp.path().join("before.fcproj")).unwrap();
    let root = folder.join(ROOT);
    let published = fs::read(&root).unwrap();
    assert!(pins::list(&folder).unwrap().is_empty());
    pins::pin(&folder, &initial, "Picture lock").unwrap();
    pins::pin(&folder, &initial, "Picture lock").unwrap();
    assert!(pins::pin(&folder, &initial, "Different milestone").is_err());
    assert_eq!(fs::read(&root).unwrap(), published);
    doc["project"]["items"]["73"]["name"] = "New ending".into();
    write_doc(&edit, &doc);
    let next = save(&folder, &initial, &edit).unwrap();
    let expected = vec![pins::Pin { revision: initial.clone(), label: "Picture lock".into() }];
    assert_eq!(pins::list(&folder).unwrap(), expected);
    assert_eq!(published_history(&folder, &backup).unwrap(), next);
    pins::unpin(&folder, &initial).unwrap();
    pins::unpin(&folder, &initial).unwrap();
    assert!(pins::list(&folder).unwrap().is_empty());
    assert_eq!(pins::list(&backup).unwrap(), expected);
    assert!(!backup.join(".revision-pins.lock").exists());
    for source in [&folder, &backup] {
        let mut saves = Vec::new();
        history(source, |_, hash| {
            saves.push(hash.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(saves, [next.clone(), initial.clone()]);
    }
}

#[test]
fn concurrent_pins_keep_both_milestones_while_the_document_writer_is_busy() {
    use converter::pins;
    use std::sync::Barrier;
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("production");
    let edit = tmp.path().join("edit.fcproj");
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("before.fcproj")).unwrap();
    doc["project"]["items"]["73"]["name"] = "Next cut".into();
    write_doc(&edit, &doc);
    let head = save(&folder, &base, &edit).unwrap();
    let writer = fs::OpenOptions::new().read(true).write(true).open(folder.join(".writer.lock")).unwrap();
    writer.try_lock().unwrap();
    let start = Barrier::new(2);
    std::thread::scope(|scope| {
        for (rev, label) in [(&base, "First cut"), (&head, "Final cut")] {
            let (start, folder) = (&start, &folder);
            scope.spawn(move || {
                start.wait();
                pins::pin(folder, rev, label).unwrap();
            });
        }
    });
    let result = pins::list(&folder).unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.iter().any(|p| p.revision == base && p.label == "First cut"));
    assert!(result.iter().any(|p| p.revision == head && p.label == "Final cut"));
}

#[test]
fn invalid_pins_do_not_replace_good_marks_or_publish_a_misleading_backup() {
    use converter::{backup::published_history, pins};
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("production");
    let backup = tmp.path().join("backup");
    let edit = tmp.path().join("edit.fcproj");
    write_doc(&edit, &current());
    pack(&edit, &folder, atomic_write_new).unwrap();
    let head = checkout(&folder, &tmp.path().join("before.fcproj")).unwrap();
    let outside = "f".repeat(64);
    let sidecar = folder.join("revision-pins.json");
    pins::pin(&folder, &head, "Review").unwrap();
    let good = fs::read(&sidecar).unwrap();
    assert!(pins::pin(&folder, &outside, "Other").is_err());
    assert!(pins::pin(&folder, &head, "  ").is_err());
    assert_eq!(fs::read(&sidecar).unwrap(), good);
    for bad in [b"broken JSON".to_vec(), serde_json::to_vec(&json!([{"revision":outside,"label":"Missing"}])).unwrap()] {
        fs::write(&sidecar, &bad).unwrap();
        assert!(pins::list(&folder).is_err());
        assert!(pins::pin(&folder, &head, "Review").is_err());
        assert!(published_history(&folder, &backup).is_err());
        assert!(!backup.exists());
        assert_eq!(fs::read(&sidecar).unwrap(), bad);
    }
}

#[test]
fn competing_milestone_renames_preserve_the_pin_and_reject_stale_labels() {
    use converter::{backup, pins, retention};
    use std::{num::NonZeroUsize, sync::Barrier};
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let edit = tmp.path().join("edit.fcproj");
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("copy.fcproj")).unwrap();
    pins::pin(&folder, &base, "Rough cut").unwrap();
    doc["project"]["items"]["73"]["name"] = "New cut".into();
    write_doc(&edit, &doc);
    let head = save(&folder, &base, &edit).unwrap();
    pins::pin(&folder, &head, "Rough cut").unwrap();

    let barrier = Barrier::new(2);
    let rename = |label| {
        barrier.wait();
        pins::rename(&folder, &base, "Rough cut", label).is_ok()
    };
    let (first, second) = std::thread::scope(|scope| {
        let a = scope.spawn(|| rename("Review A"));
        let b = scope.spawn(|| rename("Review B"));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(first, second);
    let winner = if first { "Review A" } else { "Review B" };
    let sidecar = folder.join("revision-pins.json");
    let before = fs::read(&sidecar).unwrap();
    assert!(pins::rename(&folder, &base, winner, "  ").is_err());
    pins::rename(&folder, &base, winner, winner).unwrap();
    assert_eq!(fs::read(&sidecar).unwrap(), before);

    let saved = tmp.path().join("backup");
    assert_eq!(backup::published_history(&folder, &saved).unwrap(), head);
    let report = retention::preview(&saved, NonZeroUsize::MIN, &[], &[]).unwrap();
    assert!(report.saves.iter().any(|r| r.revision == base && r.label.as_deref() == Some(winner) && r.holds.milestone && !r.holds.recent));
    assert!(report.saves.iter().any(|r| r.revision == head && r.label.as_deref() == Some("Rough cut")));
    pins::unpin(&folder, &base).unwrap();
    assert!(pins::rename(&folder, &base, winner, "Revived").is_err());
    assert_eq!(pins::list(&folder).unwrap(), [pins::Pin { revision: head, label: "Rough cut".into() }]);
}
