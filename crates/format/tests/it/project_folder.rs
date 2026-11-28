use filmcraft_format::project_folder::{ROOT, checkout, history, pack, records_from, rescue, restore_copy, save, save_with, unpack};
use filmcraft_format::{SCHEMA_VERSION, atomic_write_new};
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
        let mut expected = craft_fcproj_format::migrate_with(craft_fcproj_format::MIGRATIONS, serde_json::from_slice(&bytes).unwrap(), schema).unwrap();
        expected["schema_version"] = SCHEMA_VERSION.into();
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
fn camera_raw_settings_survive_conversion_and_a_normal_project_save() {
    use filmcraft_project::{ItemId, ItemKind, Label, ScriptDocument};
    use std::sync::Arc;

    let tmp = tempdir().unwrap();
    let mut project = filmcraft_format::decode(&fixture("v1-edit.fcproj")).unwrap().project;
    let script = ScriptDocument::from_text("A camera test").unwrap();
    let script_id = project.add_item("Script", Label::Blue, ItemKind::Script(Arc::new(script.clone())), None);
    let mut document: Value = serde_json::from_slice(&filmcraft_format::encode(&project, false)).unwrap();
    let settings = json!({
        "white_balance": {"Custom": {"temperature": 3200.0, "tint": 12.5}},
        "exposure": -0.75,
        "highlights": "Recover",
        "demosaic": "Fast",
        "output": "SceneLinearRec709"
    });
    let raw_path = "/project/items/5/kind/Media/interpret/raw";
    document.pointer_mut("/project/items/5/kind/Media/interpret").unwrap()["raw"] = settings.clone();

    for version in [18, SCHEMA_VERSION] {
        let source = tmp.path().join(format!("source-{version}.fcproj"));
        let folder = tmp.path().join(format!("folder-{version}"));
        let export = tmp.path().join(format!("export-{version}.fcproj"));
        document["schema_version"] = version.into();
        let source_bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&source, &source_bytes).unwrap();

        pack(&source, &folder, atomic_write_new).unwrap();
        unpack(&folder, &export).unwrap();
        let exported = fs::read(&export).unwrap();
        let exported_document: Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(exported_document.pointer(raw_path), Some(&settings));
        let reopened = filmcraft_format::decode(&exported).unwrap();
        assert_eq!(reopened.schema_version, SCHEMA_VERSION);
        let saved = filmcraft_format::encode_with_view(&reopened.project, reopened.view.as_ref(), false);
        fs::write(&export, saved).unwrap();
        let saved = fs::read(&export).unwrap();
        let saved_document: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(saved_document.pointer(raw_path), Some(&settings), "RAW settings lost on save of v{version} input");
        let reopened = filmcraft_format::decode(&saved).unwrap();
        let media = reopened.project.item(ItemId(5)).unwrap().as_media().unwrap();
        assert_eq!(media.interpret.raw.as_ref(), Some(&settings));
        assert_eq!(reopened.project.item(script_id).unwrap().as_script(), Some(&script));
        assert_eq!(fs::read(source).unwrap(), source_bytes);
    }
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

fn write_doc(path: &Path, doc: &Value) {
    fs::write(path, serde_json::to_vec(doc).unwrap()).unwrap();
}

#[test]
fn held_writer_lock_keeps_a_competing_save_from_publishing() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.fcproj");
    let edit = tmp.path().join("edit.fcproj");
    let folder = tmp.path().join("project");
    let mut doc = current();
    write_doc(&source, &doc);
    pack(&source, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &edit).unwrap();
    doc["project"]["items"]["73"]["name"] = json!("Revised cut");
    write_doc(&edit, &doc);

    let lock = fs::OpenOptions::new().create_new(true).read(true).write(true).open(folder.join(".writer.lock")).unwrap();
    lock.lock().unwrap();
    assert_eq!(save(&folder, &base, &edit).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    let reopened = tmp.path().join("reopened.fcproj");
    assert_eq!(checkout(&folder, &reopened).unwrap(), base);
    drop(lock);
    assert_ne!(save(&folder, &base, &edit).unwrap(), base);
}

#[test]
fn checkout_refuses_an_invalid_live_revision() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.fcproj");
    let folder = tmp.path().join("project");
    let export = tmp.path().join("export.fcproj");
    write_doc(&source, &current());
    pack(&source, &folder, atomic_write_new).unwrap();
    let root = folder.join(ROOT);
    let mut manifest: Value = serde_json::from_slice(&fs::read(&root).unwrap()).unwrap();
    manifest["layout"] = json!(2);
    manifest["revision"] = json!({"number": 1, "parent": "invalid digest", "bin_edits": {}, "item_edits": {}});
    write_doc(&root, &manifest);
    assert_eq!(checkout(&folder, &export).unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert!(!export.exists());
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
    let next = save_with(&folder, &base, records_from(&edit, &folder).unwrap(), |path, bytes| {
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
        let folder = dir.join("folder");
        let records = records_from(&dir.join("moved"), &folder).unwrap();
        let mut count = 0;
        save_with(&folder, &base, records, |path, bytes| {
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
        assert_eq!(&doc, if stop < 4 { &old } else { &moved });
        if stop < 4 {
            let next = save(&folder, &base, &dir.join("moved")).unwrap();
            let retried = dir.join("retried");
            assert_eq!(checkout(&folder, &retried).unwrap(), next);
            assert_eq!(serde_json::from_slice::<Value>(&fs::read(retried).unwrap()).unwrap(), moved);
        }
    }
}

#[test]
fn historical_cut_restores_as_a_new_sequence_and_keeps_todays_edits() {
    let tmp = tempdir().unwrap();
    let folder = tmp.path().join("project");
    let input = tmp.path().join("input");
    let mut past = current();
    let mut other_reel = past["project"]["items"]["21"].clone();
    other_reel["id"] = json!(75);
    other_reel["created"] = json!(75);
    other_reel["name"] = json!("Reel 4");
    past["project"]["items"]["75"] = other_reel;
    past["project"]["root"]["children"].as_array_mut().unwrap().push(json!({"Item": 75}));
    write_doc(&input, &past);
    pack(&input, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("base")).unwrap();
    let mut edited = past.clone();
    edited["project"]["items"]["73"]["name"] = json!("Revised cut");
    let clip = &mut edited["project"]["items"]["73"]["kind"]["Sequence"]["video_tracks"][0]["items"][0];
    clip["duration"] = json!(clip["duration"].as_i64().unwrap() / 2);
    edited["project"]["items"]["75"]["name"] = json!("Reel 4 with today's sound edits");
    let other_clip = &mut edited["project"]["items"]["75"]["kind"]["Sequence"]["audio_tracks"][0]["items"][0];
    other_clip["duration"] = json!(other_clip["duration"].as_i64().unwrap() / 3);
    write_doc(&input, &edited);
    let current_revision = save(&folder, &base, &input).unwrap();
    let (id, restored) = restore_copy(&folder, &current_revision, &base, 73).unwrap();
    assert!(edited["project"]["items"].get(id.to_string()).is_none());
    let mut revisions = Vec::new();
    history(&folder, |n, hash| {
        revisions.push((n, hash.to_owned()));
        Ok(())
    })
    .unwrap();
    assert_eq!(revisions, [(2, restored.clone()), (1, current_revision.clone()), (0, base.clone())]);
    let out = tmp.path().join("result");
    unpack(&folder, &out).unwrap();
    let result_bytes = fs::read(out).unwrap();
    let reopened = filmcraft_format::decode(&result_bytes).unwrap();
    assert_eq!(reopened.schema_version, SCHEMA_VERSION);
    assert!(reopened.project.sequence(filmcraft_project::ItemId(id)).is_some());
    let result: Value = serde_json::from_slice(&result_bytes).unwrap();
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
    let mut doc = current();
    doc["project"]["items"]["5"]["kind"]["Media"]["interpret"]["raw"] = json!({"exposure": -0.75});
    write_doc(&input, &doc);
    pack(&input, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("base")).unwrap();
    let mut edited = doc.clone();
    edited["project"]["items"]["5"]["kind"]["Media"]["interpret"]["raw"]["exposure"] = json!(1.0);
    write_doc(&input, &edited);
    let next = save(&folder, &base, &input).unwrap();
    let live_root = fs::read(folder.join(ROOT)).unwrap();
    assert!(restore_copy(&folder, &next, &base, 73).unwrap_err().to_string().contains("dependency item 5"));
    assert_eq!(fs::read(folder.join(ROOT)).unwrap(), live_root);
    let manifest: Value = serde_json::from_slice(&fs::read(folder.join(&base)).unwrap()).unwrap();
    let path_for = |group: &str, id: u64| folder.join(manifest[group].as_array().unwrap().iter().find(|row| row[0] == id).unwrap()[1].as_str().unwrap());
    fs::remove_file(path_for("items", 74)).unwrap();
    fs::remove_file(path_for("bins", 88)).unwrap();
    assert!(unpack(&folder, &tmp.path().join("all")).is_err());
    let out = tmp.path().join("rescue.fcproj");
    rescue(&folder, &base, 73, &out).unwrap();
    let bytes = fs::read(&out).unwrap();
    let loaded = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(loaded.schema_version, SCHEMA_VERSION);
    assert_eq!(loaded.project.items.keys().map(|id| id.0).collect::<Vec<_>>(), [5, 21, 73]);
    assert_eq!(loaded.view.unwrap().active_sequence, Some(filmcraft_project::ItemId(73)));
    let saved: Value = serde_json::from_slice(&bytes).unwrap();
    for id in ["5", "21", "73"] {
        assert_eq!(saved["project"]["items"][id], doc["project"]["items"][id]);
    }
    assert_eq!(fs::read(folder.join(ROOT)).unwrap(), live_root);
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

#[test]
fn native_editor_records_reopen_and_an_unedited_checkout_does_not_publish() {
    use filmcraft_format::project_folder::FolderSnapshot;
    use filmcraft_project::ItemId;

    let temp = tempdir().unwrap();
    let source = temp.path().join("source.fcproj");
    let folder = temp.path().join("folder");
    write_doc(&source, &current());
    pack(&source, &folder, atomic_write_new).unwrap();
    let (mut snapshot, opened) = FolderSnapshot::open(&folder.join(ROOT)).unwrap();
    let mut edited = opened.project.clone();
    let clip = &mut edited.sequence_mut(ItemId(73)).unwrap().video_tracks[0].items[0];
    clip.duration.0 /= 2;
    let view = opened.view.unwrap_or_default();
    let saved = snapshot.save(&opened.project, &view, &edited, &view).unwrap();

    let output = temp.path().join("checkout.fcproj");
    assert_eq!(checkout(&folder, &output).unwrap(), saved.revision);
    let reopened = filmcraft_format::decode(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(reopened.project.sequence(ItemId(73)), edited.sequence(ItemId(73)));
    assert_eq!(save(&folder, &saved.revision, &output).unwrap(), saved.revision);
}

#[test]
fn pruning_retires_only_unprotected_history_and_retries_interrupted_collection() {
    use filmcraft_format::project_folder::{RetentionPolicy, configure_retention, keep_version, preview_retention, prune_history};
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("cut.fcproj");
    let folder = tmp.path().join("folder");
    let mut doc = current();
    write_doc(&input, &doc);
    pack(&input, &folder, atomic_write_new).unwrap();
    let base = checkout(&folder, &tmp.path().join("initial")).unwrap();
    let policy = RetentionPolicy { enabled: false, max_bin_backups: 1, versions_per_bin: 1 };
    configure_retention(&folder, &base, policy).unwrap();
    keep_version(&folder, &base, &base, true).unwrap();
    let mut head = base.clone();
    let mut versions = Vec::new();
    for name in ["First", "Second", "Third", "Today"] {
        doc["project"]["items"]["73"]["name"] = json!(name);
        write_doc(&input, &doc);
        head = save(&folder, &head, &input).unwrap();
        versions.push(head.clone());
    }
    let preview = preview_retention(&folder, &head, policy).unwrap();
    assert_eq!(preview.retire, [versions[1].clone(), versions[0].clone()]);
    assert!(!preview.due);
    let archived: Value = serde_json::from_slice(&fs::read(folder.join(&versions[0])).unwrap()).unwrap();
    let victim = archived["items"].as_array().unwrap().iter().find(|v| v[0] == 73).unwrap()[1].as_str().unwrap();
    let victim_path = folder.join(victim);
    let original = fs::read(&victim_path).unwrap();
    let foreign = folder.join("a".repeat(64));
    fs::write(&foreign, b"unmanaged").unwrap();
    // A damaged record cannot be unlinked. Its pending deletion is retried
    // from authenticated history after the valid file is put back.
    fs::write(&victim_path, b"unexpected contents").unwrap();
    assert!(prune_history(&folder, &head, policy).is_err());
    assert_eq!(fs::read(&victim_path).unwrap(), b"unexpected contents");
    fs::write(&victim_path, original).unwrap();
    prune_history(&folder, &head, policy).unwrap();
    assert!(!victim_path.exists());
    assert_eq!(fs::read(foreign).unwrap(), b"unmanaged");
    assert!(rescue(&folder, &versions[0], 73, &tmp.path().join("retired")).is_err());
    rescue(&folder, &versions[2], 73, &tmp.path().join("recent")).unwrap();
    let (copy, _) = restore_copy(&folder, &head, &base, 73).unwrap();
    let output = tmp.path().join("reopen");
    unpack(&folder, &output).unwrap();
    let reopened: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(reopened["project"]["items"]["73"]["name"], "Today");
    assert_ne!(reopened["project"]["items"][copy.to_string()]["name"], "Today");
}

#[test]
fn published_backup_reopens_retained_cuts_after_pruning_and_source_removal() {
    use filmcraft_format::project_folder::{FolderSnapshot, RetentionPolicy, backup_history, keep_version, prune_history};
    use filmcraft_project::ItemId;
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("edit.fcproj");
    let source = tmp.path().join("original");
    let backup = tmp.path().join("backup");
    let mut doc = current();
    write_doc(&input, &doc);
    pack(&input, &source, atomic_write_new).unwrap();
    let (snapshot, original) = FolderSnapshot::open(&source.join(ROOT)).unwrap();
    let protected = snapshot.revision().to_owned();
    keep_version(&source, &protected, &protected, true).unwrap();
    let mut head = protected.clone();
    let mut edits = Vec::new();
    for _ in 0..4 {
        let duration = &mut doc["project"]["items"]["73"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["duration"];
        *duration = json!(duration.as_i64().unwrap() - filmcraft_time::TICKS_PER_SECOND / 24);
        write_doc(&input, &doc);
        head = save(&source, &head, &input).unwrap();
        edits.push(head.clone());
    }
    let policy = RetentionPolicy { enabled: true, max_bin_backups: 1, versions_per_bin: 1 };
    let pruned = prune_history(&source, &head, policy).unwrap();
    assert!(pruned.retire.contains(&edits[0]));
    let (_, current) = FolderSnapshot::open(&source.join(ROOT)).unwrap();
    let cut = ItemId(73);
    assert_ne!(original.project.sequence(cut), current.project.sequence(cut));

    assert_eq!(backup_history(&source, &backup).unwrap(), head);
    assert!(backup_history(&source, &backup).is_err());
    fs::remove_dir_all(&source).unwrap();
    let (snapshot, reopened) = FolderSnapshot::open(&backup.join(ROOT)).unwrap();
    assert_eq!(snapshot.revision(), head);
    assert_eq!(reopened.project.sequence(cut), current.project.sequence(cut));
    let out = tmp.path().join("protected.fcproj");
    assert!(rescue(&backup, &edits[0], cut.0, &out).is_err());
    // Prune the independent backup again to exercise the copied pin as well as visibility.
    prune_history(&backup, &head, policy).unwrap();
    rescue(&backup, &protected, cut.0, &out).unwrap();
    let restored = filmcraft_format::decode(&fs::read(out).unwrap()).unwrap();
    assert_eq!(restored.project.sequence(cut), original.project.sequence(cut));
    assert_eq!(restored.project.sequence(ItemId(21)), original.project.sequence(ItemId(21)));
}

#[test]
fn published_backup_preserves_the_cut_as_it_left_its_bin() {
    use filmcraft_format::project_folder::{Departure, DepartureCut, FolderSnapshot, TransferSave, backup_history};
    use filmcraft_project::{BinEntry, BinId, ItemId};
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("edit.fcproj");
    let source = tmp.path().join("source");
    let backup = tmp.path().join("backup");
    write_doc(&input, &current());
    pack(&input, &source, atomic_write_new).unwrap();
    let (mut snapshot, loaded) = FolderSnapshot::open(&source.join(ROOT)).unwrap();
    let base = snapshot.revision().to_owned();
    let cut = ItemId(73);
    let mut departure = loaded.project.clone();
    departure.sequence_mut(cut).unwrap().video_tracks[0].items[0].duration.0 /= 2;
    let mut moved = departure.clone();
    assert!(moved.root.remove_item(cut));
    moved.root.children.push(BinEntry::Item(cut));
    moved.sequence_mut(cut).unwrap().video_tracks[0].items[0].duration.0 /= 2;
    let moves = TransferSave {
        departures: vec![Departure {
            sequence: cut,
            source: BinId(88),
            destination: moved.root.id,
            saved_source: &base,
            cut: DepartureCut::Working(&departure),
        }],
        ..Default::default()
    };
    let view = loaded.view.unwrap_or_default();
    let saved = snapshot.save_transfers(&loaded.project, &view, &moved, &view, moves).unwrap();
    let at_move = &saved.move_backups[0].snapshot;
    assert_ne!(at_move, &base);
    backup_history(&source, &backup).unwrap();
    fs::remove_dir_all(source).unwrap();
    let out = tmp.path().join("departure.fcproj");
    rescue(&backup, at_move, cut.0, &out).unwrap();
    let restored = filmcraft_format::decode(&fs::read(out).unwrap()).unwrap();
    assert_eq!(restored.project.sequence(cut), departure.sequence(cut));
}

#[test]
fn damaged_retained_history_cannot_publish_a_backup() {
    use filmcraft_format::project_folder::backup_history;
    let tmp = tempdir().unwrap();
    let input = tmp.path().join("edit.fcproj");
    let source = tmp.path().join("original");
    let backup = tmp.path().join("backup");
    let mut doc = current();
    write_doc(&input, &doc);
    pack(&input, &source, atomic_write_new).unwrap();
    let base = checkout(&source, &tmp.path().join("first.fcproj")).unwrap();
    doc["project"]["items"]["73"]["name"] = json!("Today");
    write_doc(&input, &doc);
    let head = save(&source, &base, &input).unwrap();
    let old: Value = serde_json::from_slice(&fs::read(source.join(&base)).unwrap()).unwrap();
    let hash = old["items"].as_array().unwrap().iter().find(|v| v[0] == 73).unwrap()[1].as_str().unwrap();
    let record = source.join(hash);
    let bytes = fs::read(&record).unwrap();
    fs::write(&record, b"damaged older cut").unwrap();
    assert!(backup_history(&source, &backup).is_err());
    assert!(!backup.join(ROOT).exists());
    fs::write(record, bytes).unwrap();
    assert_eq!(backup_history(&source, &backup).unwrap(), head);
}

#[test]
fn killing_a_checkpoint_writer_on_either_side_of_publication_keeps_a_complete_draft() {
    use filmcraft_format::project_folder::{CheckpointStore, load_checkpoint};
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::Arc,
    };
    const CHILD: &str = "FILMCRAFT_TEST_CHECKPOINT_KILL";
    let edited = |name: &str| {
        let mut doc = current();
        doc["project"]["items"]["73"]["name"] = json!(name);
        doc["project"]["items"]["5"]["kind"]["Media"]["interpret"]["raw"] = json!({"exposure":-0.75});
        Arc::new(filmcraft_format::decode(&serde_json::to_vec(&doc).unwrap()).unwrap().project)
    };
    if let Some(dir) = std::env::var_os(CHILD) {
        let dir = Path::new(&dir);
        let marker = dir.join("marker");
        let mut store = CheckpointStore::default();
        for name in ["Early checkpoint", "Later checkpoint", "First checkpoint"] {
            store
                .publish(dir, &edited(name), |root, _| {
                    filmcraft_format::atomic_write(&marker, root.as_bytes())?;
                    Ok(root.len())
                })
                .unwrap();
        }
        let stop = || {
            println!("at publication boundary");
            std::thread::park();
        };
        store
            .publish(dir, &edited("Second checkpoint"), |root, _| {
                if std::env::var("FILMCRAFT_TEST_CHECKPOINT_SIDE").unwrap() == "before" {
                    stop();
                }
                filmcraft_format::atomic_write(&marker, root.as_bytes())?;
                stop();
                Ok(root.len())
            })
            .unwrap();
        return;
    }
    for (side, name) in [("before", "First checkpoint"), ("after", "Second checkpoint")] {
        let dir = tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "project_folder::killing_a_checkpoint_writer_on_either_side_of_publication_keeps_a_complete_draft", "--nocapture"])
            .env(CHILD, dir.path())
            .env("FILMCRAFT_TEST_CHECKPOINT_SIDE", side)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let reached = BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok).any(|s| s.contains("at publication boundary"));
        if reached {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        assert!(reached);
        let root = fs::read_to_string(dir.path().join("marker")).unwrap();
        let loaded = load_checkpoint(dir.path(), &root).unwrap();
        assert_eq!(&loaded.project, edited(name).as_ref());
    }
}

#[test]
fn a_trim_replaces_only_its_item_and_manifest_and_storage_full_can_retry() {
    use filmcraft_format::project_folder::{CheckpointStore, load_checkpoint};
    use filmcraft_project::ItemId;
    use std::sync::Arc;
    let dir = tempdir().unwrap();
    let marker = dir.path().join("marker");
    let publish = |root: &str, _: usize| {
        filmcraft_format::atomic_write(&marker, root.as_bytes())?;
        Ok(root.len())
    };
    let folder = dir.path().join("checkpoint-records");
    let records = |root: &str| serde_json::from_slice::<Value>(&fs::read(folder.join(root)).unwrap()).unwrap()["records"].clone();
    let mut project = Arc::new(filmcraft_format::decode(&serde_json::to_vec(&current()).unwrap()).unwrap().project);
    let mut store = CheckpointStore::default();
    let first = store.publish(dir.path(), &project, publish).unwrap();
    let old = fs::read_to_string(&marker).unwrap();
    let initial = records(&old);
    Arc::make_mut(&mut project).sequence_mut(ItemId(73)).unwrap().start_timecode = 42;
    assert_eq!(store.publish(dir.path(), &project, |_, _| Err(io::ErrorKind::StorageFull.into())).err().unwrap().kind(), io::ErrorKind::StorageFull);
    assert_eq!(fs::read_to_string(&marker).unwrap(), old);
    assert_ne!(load_checkpoint(dir.path(), &old).unwrap().project.sequence(ItemId(73)).unwrap().start_timecode, 42);
    // Advance the edit before retry so staged, unpublished payloads become garbage as well.
    Arc::make_mut(&mut project).sequence_mut(ItemId(73)).unwrap().start_timecode = 43;
    let retry = store.publish(dir.path(), &project, publish).unwrap();
    assert!(retry.cleanup_error.is_none() && retry.bytes < first.bytes);
    let new = fs::read_to_string(&marker).unwrap();
    let changed = records(&new);
    assert_eq!(initial["envelope"], changed["envelope"]);
    assert_eq!(initial["bins"], changed["bins"]);
    let changed_items: Vec<_> = initial["items"].as_array().unwrap().iter().zip(changed["items"].as_array().unwrap()).filter(|(a, b)| a != b).collect();
    assert_eq!(changed_items.len(), 1);
    assert_eq!(changed_items[0].1[0], 73);
    let encoded = serde_json::to_vec(project.item(ItemId(73)).unwrap()).unwrap();
    assert_eq!(retry.bytes, encoded.len() + fs::read(folder.join(&new)).unwrap().len() + new.len());
    assert!(!folder.join(old).exists());
    assert_eq!(load_checkpoint(dir.path(), &new).unwrap().project, *project);
    let mut sizes = Vec::new();
    for timecode in 50..70 {
        Arc::make_mut(&mut project).sequence_mut(ItemId(73)).unwrap().start_timecode = timecode;
        store.publish(dir.path(), &project, publish).unwrap();
        sizes.push(fs::metadata(folder.join("records.bin")).unwrap().len());
    }
    assert_eq!(sizes.iter().take(4).max(), sizes.iter().max(), "repeated trims reuse retired space");
    let new = fs::read_to_string(&marker).unwrap();
    assert_eq!(load_checkpoint(dir.path(), &new).unwrap().project, *project);
}

#[test]
fn editor_restore_offers_lossless_export_for_unknown_cut_fields() {
    use filmcraft_format::project_folder::{EditorRestore, FolderSnapshot, editor_restore};
    let dir = tempdir().unwrap();
    let folder = dir.path().join("project");
    let source = dir.path().join("original.fcproj");
    let mut doc = current();
    doc["project"]["items"]["73"]["kind"]["Sequence"]["extension_example"] = json!({"take":2});
    write_doc(&source, &doc);
    pack(&source, &folder, atomic_write_new).unwrap();
    let (snapshot, live) = FolderSnapshot::open(&folder.join(ROOT)).unwrap();
    let revision = snapshot.revision();
    assert!(matches!(editor_restore(&folder, revision, revision, 73, &live.project).unwrap(), EditorRestore::ExportRequired(_)));
    let output = dir.path().join("historical.fcproj");
    rescue(&folder, revision, 73, &output).unwrap();
    let recovered: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(recovered["project"]["items"]["73"], doc["project"]["items"]["73"]);
}
