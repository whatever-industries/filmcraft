use filmcraft_format::project_folder as converter;

use converter::{ROOT, pack, unpack};
use filmcraft_format::atomic_write_new;
use serde_json::Value;
use serde_json::json;
use std::{fs, io, path::Path};
use tempfile::tempdir;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

#[test]
fn legacy_roundtrips_match_ordinary_migration() {
    let tmp = tempdir().unwrap();
    for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")).unwrap() {
        let src = entry.unwrap().path();
        if src.extension().is_none_or(|ext| ext != "fcproj") {
            continue;
        }
        let folder = tmp.path().join(src.file_stem().unwrap());
        let out = tmp.path().join(src.file_name().unwrap());
        let bytes = fs::read(&src).unwrap();
        pack(&src, &folder, atomic_write_new).unwrap();
        unpack(&folder, &out).unwrap();
        let result = fs::read(out).unwrap();
        let schema = craft_fcproj_format::detect_bytes(&bytes).unwrap();
        let expected = craft_fcproj_format::migrate_with(craft_fcproj_format::MIGRATIONS, serde_json::from_slice(&bytes).unwrap(), schema).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&result).unwrap(), expected);
        assert_eq!(filmcraft_format::decode(&result).unwrap().project, filmcraft_format::decode(&bytes).unwrap().project);
    }
}

fn current() -> Value {
    let loaded = filmcraft_format::decode(&fixture("v1-edit.fcproj")).unwrap();
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&loaded.project, false)).unwrap();
    doc["extra"] = json!({"unknown":[null,{}]});
    doc["view"] = json!({"open_sequences":[21,73],"active_sequence":73});
    let p = &mut doc["project"];
    let mut seq = p["items"]["21"].clone();
    seq["id"] = json!(73);
    seq["kind"]["Sequence"]["video_tracks"][0]["items"][0]["item"] = json!(21);
    p["items"]["73"] = seq;
    let mut unfiled = p["items"]["5"].clone();
    unfiled["id"] = json!(74);
    p["items"]["74"] = unfiled;
    p["root"]["children"][0]["Bin"]["children"].as_array_mut().unwrap().push(json!({"Bin":{"id":88,"name":"Footage","children":[{"Item":73}]}}));
    p["next_id"] = json!(900000);
    doc
}

#[test]
fn offline_sources_and_proxies_follow_the_document_and_views_and_opaque_fields_survive() {
    let tmp = tempdir().unwrap();
    let base = tmp.path();
    let src = base.join("edit/cut.fcproj");
    let folder = base.join("archive/day1/cut");
    let out = base.join("delivery/restored.fcproj");
    for parent in [&src, &folder, &out] {
        fs::create_dir_all(parent.parent().unwrap()).unwrap();
    }
    let mut expected = current();
    let orig = "/project/items/5/kind/Media/media";
    let proxy = "/project/items/5/kind/Media/proxy";
    expected.pointer_mut("/project/items/5/kind/Media").unwrap()["proxy"] = json!({"File":{"path":"../proxies/p.mov"}});
    *expected.pointer_mut(orig).unwrap() = json!({"File":{"path":"media/a.mov"}});
    let bytes = serde_json::to_vec(&expected).unwrap();
    fs::write(&src, &bytes).unwrap();

    pack(&src, &folder, atomic_write_new).unwrap();
    unpack(&folder, &out).unwrap();
    assert_eq!(fs::read(&src).unwrap(), bytes);
    let result = fs::read(&out).unwrap();
    let mut actual: Value = serde_json::from_slice(&result).unwrap();
    let view = filmcraft_format::decode(&result).unwrap().view.unwrap();
    assert_eq!(view.active_sequence, Some(filmcraft_project::ItemId(73)));
    assert_eq!(view, filmcraft_format::decode(&bytes).unwrap().view.unwrap());
    for ptr in [orig, proxy] {
        let value = actual.pointer_mut(ptr).unwrap();
        assert_eq!(
            craft_docpath::resolve_absolute(value["File"]["path"].as_str().unwrap(), Some(&out)).unwrap(),
            craft_docpath::resolve_absolute(expected.pointer(ptr).unwrap()["File"]["path"].as_str().unwrap(), Some(&src)).unwrap()
        );
        *value = expected.pointer(ptr).unwrap().clone();
    }
    assert_eq!(actual, expected);
}

#[test]
fn occupied_destinations_and_write_failures_preserve_existing_data() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("input.fcproj");
    let bytes = fixture("v1-edit.fcproj");
    fs::write(&src, &bytes).unwrap();
    let mut perms = fs::metadata(&src).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&src, perms).unwrap();
    let folder = tmp.path().join("folder");
    fs::create_dir(&folder).unwrap();
    assert_eq!(pack(&src, &folder, atomic_write_new).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    for fail_at in [1, 3] {
        let dest = tmp.path().join(format!("failed-{fail_at}"));
        let mut count = 0;
        let err = pack(&src, &dest, |p, bytes| {
            count += 1;
            if count == fail_at {
                return Err(io::Error::from(io::ErrorKind::StorageFull));
            }
            atomic_write_new(p, bytes)
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
        assert!(!dest.join(ROOT).exists());
        assert!(unpack(&dest, &tmp.path().join("should-not-exist")).is_err());
    }
    let dest = tmp.path().join("complete");
    pack(&src, &dest, atomic_write_new).unwrap();
    assert_eq!(unpack(&dest, &src).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&src).unwrap(), bytes);
}

#[test]
fn damaged_missing_and_unknown_records_refuse_export() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("input");
    fs::write(&src, fixture("v1-edit.fcproj")).unwrap();
    let dir = tmp.path().join("dir");
    pack(&src, &dir, atomic_write_new).unwrap();
    let root = dir.join(ROOT);
    let mut m: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    let file = dir.join(m["items"][0][1].as_str().unwrap());
    let out = tmp.path().join("export");
    fs::write(&file, b"{}").unwrap();
    for _ in 0..2 {
        let error = unpack(&dir, &out).unwrap_err().to_string();
        assert!(error.contains(&format!("item {}", m["items"][0][0])), "{error}");
        assert!(!out.exists());
        if file.exists() {
            fs::remove_file(&file).unwrap();
        }
    }
    m["layout"] = json!(2);
    fs::write(root, serde_json::to_vec(&m).unwrap()).unwrap();
    assert!(unpack(&dir, &out).unwrap_err().to_string().contains("unsupported"));
}

#[test]
fn verification_precedes_commit_and_interrupted_publication_is_unreadable() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::write(&src, fixture("v1-edit.fcproj")).unwrap();
    for corrupt in [true, false] {
        let dir = tmp.path().join(format!("dest-{corrupt}"));
        let result = pack(&src, &dir, |p, bytes| {
            if corrupt {
                atomic_write_new(p, b"corrupted")
            } else if p.file_name().unwrap() == ROOT {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                atomic_write_new(p, bytes)
            }
        });
        assert!(result.is_err());
        assert!(!dir.join(ROOT).exists());
    }
}

#[cfg(unix)]
#[test]
fn foreign_absolute_paths_are_preserved_in_both_directions() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let folder = tmp.path().join("folder");
    let out = tmp.path().join("out");
    let mut doc = current();
    let media = doc.pointer_mut("/project/items/5/kind/Media").unwrap();
    media["media"] = json!({"File":{"path":r"C:\Camera\A.mov"}});
    media["proxy"] = json!({"File":{"path":r"\\server\media\A.mov"}});
    fs::write(&src, serde_json::to_vec(&doc).unwrap()).unwrap();
    pack(&src, &folder, atomic_write_new).unwrap();
    unpack(&folder, &out).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&fs::read(out).unwrap()).unwrap(), doc);
}

#[test]
fn invalid_inputs_never_publish_a_destination() {
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("input.fcproj");
    let folder = tmp.path().join("folder");
    let mut project = current();
    project["schema_version"] = json!(filmcraft_format::SCHEMA_VERSION + 1);
    fs::write(&input, serde_json::to_vec(&project).unwrap()).unwrap();
    assert!(pack(&input, &folder, atomic_write_new).is_err());
    assert!(!folder.exists());

    fs::write(&input, fixture("v1-edit.fcproj")).unwrap();
    pack(&input, &folder, atomic_write_new).unwrap();
    let root = folder.join(ROOT);
    let original: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    let mut repeated = original.clone();
    repeated["items"].as_array_mut().unwrap().push(original["items"][0].clone());
    let mut wrong_id = original.clone();
    wrong_id["items"][0][0] = json!(900000);
    let mut invalid_digest = original;
    invalid_digest["envelope"] = json!("../input.fcproj");
    let out = tmp.path().join("output.fcproj");
    for manifest in [repeated, wrong_id, invalid_digest] {
        fs::write(&root, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(unpack(&folder, &out).is_err());
        assert!(!out.exists());
    }
}

#[test]
fn editing_one_sequence_changes_only_its_record() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.fcproj");
    let mut project = current();
    let write_snapshot = |name: &str, doc: &Value| {
        fs::write(&source, serde_json::to_vec(doc).unwrap()).unwrap();
        let dir = tmp.path().join(name);
        pack(&source, &dir, atomic_write_new).unwrap();
        serde_json::from_slice::<Value>(&fs::read(dir.join(ROOT)).unwrap()).unwrap()
    };
    let first = write_snapshot("first", &project);
    project["project"]["items"]["73"]["name"] = json!("Revised cut");
    let second = write_snapshot("second", &project);
    assert_eq!(first["bins"], second["bins"]);
    assert_eq!(first["envelope"], second["envelope"]);
    let items = |v: &Value| {
        let entries: Vec<(u64, String)> = serde_json::from_value(v["items"].clone()).unwrap();
        entries.into_iter().collect::<std::collections::BTreeMap<_, _>>()
    };
    let mut old = items(&first);
    let mut new = items(&second);
    let old_cut = old.remove(&73).unwrap();
    let new_cut = new.remove(&73).unwrap();
    assert_ne!(old_cut, new_cut);
    assert_eq!(old, new);
    let cut: Value = serde_json::from_slice(&fs::read(tmp.path().join("second").join(new_cut)).unwrap()).unwrap();
    assert_eq!(cut, project["project"]["items"]["73"]);
}

use converter::revisions::{checkout, save, save_with};
use craft_fcproj_format::records::ProjectRecords;

fn write_doc(path: &Path, doc: &Value) {
    fs::write(path, serde_json::to_vec(doc).unwrap()).unwrap();
}

#[test]
fn trim_publishes_only_its_record_and_revision_metadata_and_rejects_a_stale_editor() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src.fcproj");
    let folder = tmp.path().join("folder");
    let edit = tmp.path().join("edit.fcproj");
    let mut doc = current();
    write_doc(&src, &doc);
    pack(&src, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &edit).unwrap();
    let first: Value = serde_json::from_slice(&fs::read(folder.join(ROOT)).unwrap()).unwrap();
    assert_eq!(save(&folder, &base, &edit).unwrap(), base);

    let duration = &mut doc["project"]["items"]["73"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["duration"];
    *duration = json!(duration.as_i64().unwrap() / 2);
    write_doc(&edit, &doc);
    let mut writes = Vec::new();
    let next = save_with(&folder, &base, ProjectRecords::split(&fs::read(&edit).unwrap()).unwrap(), |path, bytes| {
        writes.push((path.file_name().unwrap().to_owned(), bytes.len()));
        if path.ends_with(ROOT) { filmcraft_format::atomic_write(path, bytes) } else { atomic_write_new(path, bytes) }
    })
    .unwrap();
    let second: Value = serde_json::from_slice(&fs::read(folder.join(ROOT)).unwrap()).unwrap();
    let cut = second["items"].as_array().unwrap().iter().find(|r| r[0] == 73).unwrap();
    let cut_size = serde_json::to_vec(&doc["project"]["items"]["73"]).unwrap().len();
    assert_eq!(
        writes,
        vec![
            (cut[1].as_str().unwrap().into(), cut_size),
            (base.as_str().into(), serde_json::to_vec(&first).unwrap().len()),
            (ROOT.into(), serde_json::to_vec(&second).unwrap().len())
        ]
    );
    assert_eq!(second["bins"], first["bins"]);
    assert_eq!(second["envelope"], first["envelope"]);
    assert_eq!(second["revision"]["item_edits"].as_object().unwrap().len(), 1);
    let stamp = second["revision"]["item_edits"]["73"].clone();
    assert!(stamp["utc_ms"].as_i64().unwrap() > 0);
    assert_eq!(stamp["revision"], 1);
    assert_eq!(second["revision"]["parent"], base);
    assert!(save(&folder, &base, &src).unwrap_err().to_string().contains("stale"));

    doc["project"]["items"]["21"]["name"] = json!("Another reel");
    write_doc(&edit, &doc);
    save(&folder, &next, &edit).unwrap();
    let third: Value = serde_json::from_slice(&fs::read(folder.join(ROOT)).unwrap()).unwrap();
    assert_eq!(third["revision"]["item_edits"]["73"], stamp);
    assert_eq!(third["revision"]["item_edits"]["21"]["revision"], 2);
}

#[test]
fn moving_a_sequence_survives_writer_death_at_each_publication_boundary() {
    // The subprocess exits without running any Rust destructors, including the writer guard.
    const CHILD: &str = "FILMCRAFT_FOLDER_MOVE_EXIT_TEST";
    if let Some(dir) = std::env::var_os(CHILD) {
        let dir = Path::new(&dir);
        let base = fs::read_to_string(dir.join("base")).unwrap();
        let stop: usize = fs::read_to_string(dir.join("stop")).unwrap().parse().unwrap();
        let records = ProjectRecords::split(&fs::read(dir.join("moved")).unwrap()).unwrap();
        let mut count = 0;
        save_with(&dir.join("folder"), &base, records, |path, bytes| {
            if count == stop {
                std::process::exit(85)
            }
            if path.ends_with(ROOT) {
                filmcraft_format::atomic_write(path, bytes)?;
            } else {
                atomic_write_new(path, bytes)?;
            }
            count += 1;
            if path.ends_with(ROOT) {
                std::process::exit(86)
            }
            Ok(())
        })
        .unwrap();
        panic!("child missed commit");
    }
    for stop in 0..=4 {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        let folder = dir.join("folder");
        let old = current();
        write_doc(&dir.join("input"), &old);
        pack(&dir.join("input"), &folder, atomic_write_new).unwrap();
        let base = checkout(&folder, &dir.join("initial")).unwrap();
        fs::write(dir.join("base"), &base).unwrap();
        fs::write(dir.join("stop"), stop.to_string()).unwrap();
        let mut moved = old.clone();
        let bin = &mut moved["project"]["root"]["children"][0]["Bin"];
        let list = bin["children"].as_array_mut().unwrap();
        let nested = list.iter_mut().find_map(|v| v.get_mut("Bin")).unwrap();
        let item = nested["children"].as_array_mut().unwrap().pop().unwrap();
        list.push(item);
        write_doc(&dir.join("moved"), &moved);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "project_folder::moving_a_sequence_survives_writer_death_at_each_publication_boundary"])
            .env(CHILD, dir)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(if stop < 4 { 85 } else { 86 }));
        let reopened = dir.join("reopened");
        unpack(&folder, &reopened).unwrap();
        let doc: Value = serde_json::from_slice(&fs::read(reopened).unwrap()).unwrap();
        assert_eq!(doc, if stop < 4 { old } else { moved });
        if stop < 4 {
            save(&folder, &base, &dir.join("moved")).unwrap();
        }
    }
}

use converter::recovery::{history, rescue, restore_copy};

#[test]
fn historical_cut_restores_as_a_new_item_without_replacing_the_current_cut() {
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let input = tmp.path().join("input");
    let past = current();
    write_doc(&input, &past);
    pack(&input, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("base")).unwrap();
    let mut edited = past.clone();
    edited["project"]["items"]["73"]["name"] = json!("Revised cut");
    let clip = &mut edited["project"]["items"]["73"]["kind"]["Sequence"]["video_tracks"][0]["items"][0];
    clip["duration"] = json!(clip["duration"].as_i64().unwrap() / 2);
    write_doc(&input, &edited);
    let current_revision = save(&folder, &base, &input).unwrap();
    let (id, restored) = restore_copy(&folder, &current_revision, &base, 73).unwrap();
    assert!(id >= 900000);
    let mut revisions = Vec::new();
    history(&folder, |n, hash| {
        revisions.push((n, hash.to_owned()));
        Ok(())
    })
    .unwrap();
    assert_eq!(revisions, [(2, restored.clone()), (1, current_revision.clone()), (0, base.clone())]);
    let out = tmp.path().join("result");
    unpack(&folder, &out).unwrap();
    let result: Value = serde_json::from_slice(&fs::read(out).unwrap()).unwrap();
    let mut expected = edited;
    let mut copy = past["project"]["items"]["73"].clone();
    copy["id"] = json!(id);
    copy["created"] = json!(id);
    copy["name"] = json!(format!("{} (Recovered)", copy["name"].as_str().unwrap()));
    expected["project"]["items"][id.to_string()] = copy;
    let nested = expected["project"]["root"]["children"][0]["Bin"]["children"].as_array_mut().unwrap().iter_mut().find_map(|v| v.get_mut("Bin")).unwrap();
    nested["children"].as_array_mut().unwrap().push(json!({"Item":id}));
    expected["project"]["next_id"] = json!(id + 1);
    assert_eq!(result, expected);
    assert!(restore_copy(&folder, &current_revision, &base, 73).unwrap_err().to_string().contains("stale"));
    assert!(restore_copy(&folder, &restored, &base, 5).is_err());
    fs::write(folder.join(&base), b"damaged").unwrap();
    assert!(restore_copy(&folder, &restored, &base, 73).unwrap_err().to_string().contains("damaged archived manifest"));
    assert_eq!(checkout(&folder, &tmp.path().join("after")).unwrap(), restored);
}

#[test]
fn rescue_recovers_nested_sources_despite_unrelated_damage_but_refuses_missing_required_items() {
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let input = tmp.path().join("input");
    let doc = current();
    write_doc(&input, &doc);
    pack(&input, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("base")).unwrap();
    let mut edited = doc.clone();
    edited["project"]["items"]["5"]["name"] = json!("Replacement footage");
    write_doc(&input, &edited);
    let next = save(&folder, &base, &input).unwrap();
    assert!(restore_copy(&folder, &next, &base, 73).unwrap_err().to_string().contains("dependency item 5"));
    let manifest: Value = serde_json::from_slice(&fs::read(folder.join(&base)).unwrap()).unwrap();
    let path_for = |group: &str, id: u64| folder.join(manifest[group].as_array().unwrap().iter().find(|row| row[0] == id).unwrap()[1].as_str().unwrap());
    fs::remove_file(path_for("items", 74)).unwrap();
    fs::remove_file(path_for("bins", 88)).unwrap();
    assert!(unpack(&folder, &tmp.path().join("all")).is_err());
    let out = tmp.path().join("rescue.fcproj");
    rescue(&folder, &base, 73, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let loaded = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(loaded.project.items.keys().map(|id| id.0).collect::<Vec<_>>(), [5, 21, 73]);
    assert_eq!(loaded.view.unwrap().active_sequence, Some(filmcraft_project::ItemId(73)));
    let saved: Value = serde_json::from_slice(&bytes).unwrap();
    for id in ["5", "21", "73"] {
        assert_eq!(saved["project"]["items"][id], doc["project"]["items"][id]);
    }
    assert!(rescue(&folder, &base, 73, &out).is_err());
    fs::write(path_for("items", 5), b"damaged").unwrap();
    let refused = tmp.path().join("refused");
    assert!(rescue(&folder, &base, 73, &refused).unwrap_err().to_string().contains("item 5"));
    assert!(!refused.exists());
    assert_eq!(fs::read(out).unwrap(), bytes);
}

#[test]
fn a_deleted_sequence_recovers_into_the_root_and_corrupt_history_cannot_be_used() {
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let input = tmp.path().join("input");
    let mut doc = current();
    write_doc(&input, &doc);
    pack(&input, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("base")).unwrap();
    doc["project"]["items"].as_object_mut().unwrap().remove("73");
    let bins = doc["project"]["root"]["children"][0]["Bin"]["children"].as_array_mut().unwrap();
    bins.retain(|entry| entry.get("Bin").is_none());
    write_doc(&input, &doc);
    let deleted = save(&folder, &base, &input).unwrap();
    let (id, next) = restore_copy(&folder, &deleted, &base, 73).unwrap();
    let result = tmp.path().join("result");
    unpack(&folder, &result).unwrap();
    let out: Value = serde_json::from_slice(&fs::read(result).unwrap()).unwrap();
    assert!(out["project"]["items"].get("73").is_none());
    assert_eq!(out["project"]["root"]["children"].as_array().unwrap().last(), Some(&json!({"Item":id})));
    let archived = folder.join(&base);
    let root = fs::read(folder.join(ROOT)).unwrap();
    fs::write(&archived, &root).unwrap();
    let error = restore_copy(&folder, &next, &base, 73).unwrap_err();
    assert!(error.to_string().contains("damaged archived manifest"));
    assert_eq!(fs::read(folder.join(ROOT)).unwrap(), root);
}

#[path = "bins.rs"]
mod bin_recovery;

#[test]
fn an_untouched_record_keeps_its_original_bytes_and_digest() {
    use sha2::{Digest, Sha256};
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("cut");
    let src = tmp.path().join("ordinary.fcproj");
    fs::write(&src, serde_json::to_vec(&current()).unwrap()).unwrap();
    pack(&src, &folder, atomic_write_new).unwrap();
    let root = folder.join(ROOT);
    let mut manifest: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    let entry = &mut manifest["items"][0];
    let record: Value = serde_json::from_slice(&fs::read(folder.join(entry[1].as_str().unwrap())).unwrap()).unwrap();
    let pretty = serde_json::to_vec_pretty(&record).unwrap();
    let hash = format!("{:x}", Sha256::digest(&pretty));
    fs::write(folder.join(&hash), &pretty).unwrap();
    entry[1] = json!(hash);
    fs::write(&root, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let out = tmp.path().join("checkout.fcproj");
    let base = checkout(&folder, &out).unwrap();
    let mut doc: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    doc["project"]["root"]["name"] = json!("Edited");
    fs::write(&out, serde_json::to_vec(&doc).unwrap()).unwrap();
    assert_ne!(save(&folder, &base, &out).unwrap(), base);
    let next: Value = serde_json::from_slice(&fs::read(root).unwrap()).unwrap();
    assert_eq!(next["items"], manifest["items"]);
    assert_eq!(fs::read(folder.join(hash)).unwrap(), pretty);
}

#[test]
fn published_backup_recovers_old_cuts_without_the_source_and_excludes_private_files() {
    use converter::{backup::published_history, recovery};
    use std::collections::BTreeSet;
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("production");
    let backup = tmp.path().join("backup");
    let edit = tmp.path().join("cut.fcproj");
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let root = folder.join(ROOT);
    // Preserve noncanonical archived manifests too: their bytes determine their identity.
    let initial: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    fs::write(&root, serde_json::to_vec_pretty(&initial).unwrap()).unwrap();
    let base = checkout(&folder, &tmp.path().join("checkout.fcproj")).unwrap();
    let old_name = doc["project"]["items"]["73"]["name"].as_str().unwrap().to_owned();
    doc["project"]["items"]["73"]["name"] = "New cut".into();
    write_doc(&edit, &doc);
    let head = save(&folder, &base, &edit).unwrap();
    let orphan = "f".repeat(64);
    for name in [".writer.lock", ".private-checkpoint", "footage.mov", &orphan] {
        fs::write(folder.join(name), b"exclude").unwrap();
    }
    let latest = fs::read(&root).unwrap();
    assert_eq!(published_history(&folder, &backup).unwrap(), head);
    assert_eq!(fs::read(&root).unwrap(), latest);
    assert_eq!(fs::read(backup.join(ROOT)).unwrap(), latest);
    let mut expected = BTreeSet::from([ROOT.to_owned(), base.clone()]);
    for m in [initial, serde_json::from_slice(&latest).unwrap()] {
        expected.insert(m["envelope"].as_str().unwrap().into());
        for kind in ["items", "bins"] {
            expected.extend(m[kind].as_array().unwrap().iter().map(|row| row[1].as_str().unwrap().to_owned()));
        }
    }
    let members = fs::read_dir(&backup).unwrap().map(|p| p.unwrap().file_name().into_string().unwrap()).collect::<BTreeSet<_>>();
    assert_eq!(members, expected);
    for name in members {
        assert_eq!(fs::read(folder.join(&name)).unwrap(), fs::read(backup.join(name)).unwrap());
    }
    fs::remove_dir_all(&folder).unwrap();
    for (revision, name) in [(base, old_name.as_str()), (head, "New cut")] {
        let out = tmp.path().join(format!("{revision}.fcproj"));
        recovery::rescue(&backup, &revision, 73, &out).unwrap();
        let restored = filmcraft_format::decode(&fs::read(out).unwrap()).unwrap().project;
        assert_eq!(restored.item(filmcraft_project::ItemId(73)).unwrap().name, name);
        assert!(restored.item(filmcraft_project::ItemId(5)).is_some());
    }
}

#[test]
fn backup_rejects_an_occupied_destination_and_damage_unique_to_an_older_save() {
    use converter::backup::published_history;
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("production");
    let backup = tmp.path().join("backup");
    let edit = tmp.path().join("cut.fcproj");
    let mut doc = current();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    assert!(published_history(&folder, &folder.join("nested")).is_err());
    assert!(!folder.join("nested").exists());
    fs::create_dir(&backup).unwrap();
    fs::write(backup.join(ROOT), b"keep").unwrap();
    assert_eq!(published_history(&folder, &backup).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(backup.join(ROOT)).unwrap(), b"keep");

    let base = checkout(&folder, &tmp.path().join("before.fcproj")).unwrap();
    let root = folder.join(ROOT);
    let old: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    let original = old["items"].as_array().unwrap().iter().find(|r| r[0] == 73).unwrap()[1].as_str().unwrap();
    doc["project"]["items"]["73"]["name"] = "Updated cut".into();
    write_doc(&edit, &doc);
    save(&folder, &base, &edit).unwrap();
    let latest = fs::read(&root).unwrap();
    fs::write(folder.join(original), b"damaged").unwrap();
    unpack(&folder, &tmp.path().join("still-opens.fcproj")).unwrap();
    let fresh = tmp.path().join("attempt");
    let message = published_history(&folder, &fresh).unwrap_err().to_string();
    assert!(message.contains(&base) && message.contains("item 73"), "{message}");
    assert!(!fresh.exists());
    assert_eq!(fs::read(root).unwrap(), latest);
}

#[test]
fn availability_follows_the_selected_save_and_the_restored_folder() {
    use converter::media_availability::{Availability::*, inspect};
    let tmp = tempdir().unwrap();
    let original = tmp.path().join("production");
    let restored = tmp.path().join("restored");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&restored).unwrap();
    let edit = original.join("edit.fcproj");
    let folder = original.join("folder");
    let backup = restored.join("folder");
    let mut doc = current();
    let media = &mut doc["project"]["items"]["5"]["kind"]["Media"];
    media["media"] = json!({"File":{"path":"take.mov"}});
    media["proxy"] = json!({"File":{"path":"proxy.mov"}});
    media["offline"] = true.into();
    write_doc(&edit, &doc);
    fs::write(original.join("take.mov"), b"original").unwrap();
    pack(&edit, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &original.join("checkout.fcproj")).unwrap();
    doc["project"]["items"]["5"]["kind"]["Media"]["media"]["File"]["path"] = "later.mov".into();
    write_doc(&edit, &doc);
    let head = save(&folder, &base, &edit).unwrap();
    assert_eq!(converter::backup::published_history(&folder, &backup).unwrap(), head);
    let before = inspect(&folder, &base).unwrap();
    assert!(before.item_errors.is_empty());
    assert_eq!(before.sources.iter().map(|s| (s.id, s.availability)).collect::<Vec<_>>(), [(5, OriginalAvailable), (74, Generated)]);
    assert!(before.sources[0].marked_offline);
    assert_eq!(inspect(&folder, &head).unwrap().sources[0].availability, NeedsRelink);
    assert_eq!(inspect(&backup, &base).unwrap().sources[0].availability, NeedsRelink);
    fs::write(restored.join("proxy.mov"), b"proxy").unwrap();
    assert_eq!(inspect(&backup, &base).unwrap().counts[&ProxyOnly], 1);
    fs::write(restored.join("take.mov"), b"restored take").unwrap();
    assert_eq!(inspect(&backup, &base).unwrap().sources[0].availability, OriginalAvailable);
    assert_eq!(inspect(&backup, &head).unwrap().sources[0].availability, ProxyOnly);
}

#[test]
fn availability_reports_refused_paths_and_reads_around_damaged_items() {
    use converter::media_availability::{Availability::*, Probe, inspect};
    let tmp = tempdir().unwrap();
    let edit = tmp.path().join("edit.fcproj");
    let folder = tmp.path().join("folder");
    let mut doc = current();
    let media = &mut doc["project"]["items"]["5"]["kind"]["Media"];
    media["media"] = json!({"File":{"path":"https://example.invalid/shot.mov"}});
    media["proxy"] = json!({"File":{"path":"directory"}});
    fs::create_dir(tmp.path().join("directory")).unwrap();
    write_doc(&edit, &doc);
    pack(&edit, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("checkout.fcproj")).unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(folder.join(ROOT)).unwrap()).unwrap();
    let broken = manifest["items"].as_array().unwrap().iter().find(|entry| entry[0] == 74).unwrap();
    fs::write(folder.join(broken[1].as_str().unwrap()), b"corrupt").unwrap();
    let report = inspect(&folder, &base).unwrap();
    assert_eq!(report.item_errors.keys().copied().collect::<Vec<_>>(), [74]);
    assert_eq!(report.sources.len(), 1);
    let source = &report.sources[0];
    assert_eq!((source.id, source.availability), (5, NeedsInspection));
    assert!(matches!(&source.original, Probe::Refused { path, .. } if path.starts_with("https:")));
    assert!(matches!(&source.proxy, Some(Probe::Inaccessible { reason, .. }) if reason.contains("regular file")));
}

#[path = "pins.rs"]
mod revision_pins;
