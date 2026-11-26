use super::*;
use converter::{
    FolderSnapshot,
    checkpoint::{Checkpoint, Source},
    pins,
    retention::preview,
};
use std::{collections::BTreeMap, num::NonZeroUsize};

fn snapshot(folder: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect()
}

#[test]
fn retention_counts_shared_bytes_once_and_protects_reader_draft_and_pin_independently() {
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let edit = tmp.path().join("edit.fcproj");
    let draft = tmp.path().join("session");
    fs::create_dir(&draft).unwrap();
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let mut head = checkout(&folder, &tmp.path().join("copy.fcproj")).unwrap();
    pins::pin(&folder, &head, "Approval").unwrap();
    let mut saves = vec![head.clone()];
    doc["project"]["items"]["73"]["name"] = "Reader cut".into();
    write_doc(&edit, &doc);
    head = save(&folder, &head, &edit).unwrap();
    saves.push(head.clone());
    let (reader, _) = FolderSnapshot::open_shared(&folder.join(ROOT)).unwrap();
    for name in ["Draft base", "Unused version", "Current cut"] {
        doc["project"]["items"]["73"]["name"] = name.into();
        write_doc(&edit, &doc);
        head = save(&folder, &head, &edit).unwrap();
        saves.push(head.clone());
    }
    let source = Source::capture(&folder.canonicalize().unwrap(), &saves[2]).unwrap();
    Checkpoint::publish(&draft, source, 1, "Private edits", &fs::read(&edit).unwrap()).unwrap();
    let (writer, _) = FolderSnapshot::open(&folder.join(ROOT)).unwrap();
    assert_eq!(reader.revision(), saves[1]);
    assert!(writer.writable() && !reader.writable());
    let loose_bytes = b"could belong to an unfinished save";
    fs::write(folder.join("e".repeat(64)), loose_bytes).unwrap();
    let before = snapshot(&folder);
    let private = snapshot(&draft);
    let keep = NonZeroUsize::MIN;
    let plain = preview(&folder, keep, &[], &[]).unwrap();
    let report = preview(&folder, keep, &[reader.revision().to_owned()], std::slice::from_ref(&draft)).unwrap();
    assert_eq!(report.head, head);
    assert_eq!(report.saves[4].label.as_deref(), Some("Approval"));
    assert!(report.saves[3].holds.supplied_reader);
    assert!(report.saves[2].holds.supplied_draft);
    assert!(report.saves[0].holds.recent && !report.saves[0].holds.supplied_reader);
    assert!(plain.older_only_payload_bytes > report.older_only_payload_bytes);
    let item_hash = |revision: &str, id: u64| -> String {
        let file = if revision == head { ROOT } else { revision };
        let manifest: Value = serde_json::from_slice(&fs::read(folder.join(file)).unwrap()).unwrap();
        manifest["items"].as_array().unwrap().iter().find(|row| row[0] == id).unwrap()[1].as_str().unwrap().to_owned()
    };
    let old = item_hash(&saves[3], 73);
    assert_eq!(report.older_only_payload_bytes, fs::metadata(folder.join(old)).unwrap().len());
    let shared = &report.members[&item_hash(&head, 5)];
    assert_eq!(shared.payload_revision_count, saves.len());
    assert!(shared.holds.recent && shared.holds.milestone && shared.holds.supplied_reader && shared.holds.supplied_draft);
    assert!(saves[..4].iter().all(|s| report.members[s].manifest_chain));
    let disk_bytes: u64 = before.iter().filter(|(name, _)| name.as_encoded_bytes().len() == 64 || *name == ROOT).map(|(_, bytes)| bytes.len() as u64).sum();
    assert_eq!(disk_bytes, report.retained_policy_bytes + report.older_only_payload_bytes + report.unreferenced_digest_bytes);
    assert_eq!(report.unreferenced_digest_files, 1);
    assert_eq!(report.unreferenced_digest_bytes, loose_bytes.len() as u64);
    assert_eq!(report.confirmed_reclaimable_bytes, 0);
    assert_eq!(preview(&folder, NonZeroUsize::new(5).unwrap(), &[], &[]).unwrap().older_only_payload_bytes, 0);
    assert_eq!(snapshot(&folder), before);
    assert_eq!(snapshot(&draft), private);
}

#[test]
fn retention_refuses_unknown_bases_foreign_drafts_and_damaged_payloads() {
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let backup = tmp.path().join("backup");
    let edit = tmp.path().join("edit.fcproj");
    write_doc(&edit, &current());
    pack(&edit, &folder, atomic_write_new).unwrap();
    let head = converter::backup::published_history(&folder, &backup).unwrap();
    let dir = tmp.path().join("session");
    fs::create_dir(&dir).unwrap();
    Checkpoint::publish(&dir, Source::capture(&backup.canonicalize().unwrap(), &head).unwrap(), 1, "Other copy", &fs::read(edit).unwrap()).unwrap();
    assert!(preview(&folder, NonZeroUsize::MIN, &[], &[dir]).is_err());
    assert!(preview(&folder, NonZeroUsize::MIN, &["e".repeat(64)], &[]).is_err());
    let manifest: Value = serde_json::from_slice(&fs::read(folder.join(ROOT)).unwrap()).unwrap();
    let hash = manifest["items"][0][1].as_str().unwrap();
    fs::write(folder.join(hash), b"damaged").unwrap();
    let error = preview(&folder, NonZeroUsize::MIN, &[], &[]).err().unwrap().to_string();
    assert!(error.contains(hash) && error.contains(&head));
}

#[test]
fn profile_scan_protects_the_original_projects_drafts_and_reports_unreadable_journals() {
    use converter::retention::draft_sessions;
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let backup = tmp.path().join("backup");
    let profile = tmp.path().join("profile");
    let sessions = filmcraft_format::autosave::recovery_root(&converter::checkpoint::data_directory(&profile));
    let edit = tmp.path().join("edit.fcproj");
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let base = converter::backup::published_history(&folder, &backup).unwrap();
    doc["project"]["items"]["73"]["name"] = "Edited cut".into();
    write_doc(&edit, &doc);
    save(&folder, &base, &edit).unwrap();

    let scan = || draft_sessions(&sessions, &folder);
    assert!(scan().unwrap().is_none());
    fs::create_dir_all(sessions.join("standard-file-session")).unwrap();
    assert!(scan().unwrap().unwrap().is_empty());
    let draft = sessions.join("working");
    let other = sessions.join("independent-copy");
    let bytes = fs::read(&edit).unwrap();
    for (project, dir) in [(&folder, &draft), (&backup, &other)] {
        fs::create_dir(dir).unwrap();
        Checkpoint::publish(dir, Source::capture(&project.canonicalize().unwrap(), &base).unwrap(), 1, "Private cut", &bytes).unwrap();
    }
    let before = snapshot(&draft);
    let found = scan().unwrap().unwrap();
    assert_eq!(found.as_slice(), std::slice::from_ref(&draft));
    let report = preview(&folder, NonZeroUsize::MIN, &[], &found).unwrap();
    assert!(report.saves.iter().any(|s| s.revision == base && s.holds.supplied_draft && !s.holds.recent));
    assert_eq!(report.confirmed_reclaimable_bytes, 0);
    assert_eq!(before, snapshot(&draft));

    fs::write(other.join(converter::checkpoint::DESCRIPTOR), b"broken").unwrap();
    let error = scan().unwrap_err();
    assert!(error.to_string().contains("independent-copy"));
}
