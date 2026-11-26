use crate::Session;
use filmcraft_format::project_folder::{ROOT, pack};
use filmcraft_project::{BinEntry, BinId, ItemId};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn fixture(parent: &Path, media: &Path) -> PathBuf {
    fs::create_dir_all(parent).unwrap();
    let p = filmcraft_format::decode(include_bytes!("../../../format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&p, false)).unwrap();
    let project = &mut doc["project"];
    project["root"]["children"] = json!([
        {"Bin":{"id":70,"name":"Assembly","children":[{"Item":21},{"Bin":{"id":72,"name":"Empty","children":[]}}]}},
        {"Bin":{"id":71,"name":"Selects","children":[{"Item":81}]}},{"Item":5}]);
    project["items"]["81"] = project["items"]["21"].clone();
    project["items"]["81"]["id"] = 81.into();
    project["items"]["5"]["kind"]["Media"]["media"] = json!({"File":{"path":media.join("original.mov")}});
    project["items"]["5"]["kind"]["Media"]["proxy"] = json!({"File":{"path":media.join("proxy.mov")}});
    project["items"]["21"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["extension"] = json!({"slate":9});
    project["next_id"] = 2000.into();
    let file = parent.join("fixture.fcproj");
    fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
    let folder = parent.join("folder");
    pack(&file, &folder, filmcraft_format::atomic_write_new).unwrap();
    folder
}

fn open(folder: &Path) -> Session {
    let mut s = Session::local_folder_editor();
    s.execute("file.open", json!({"path":folder.join(ROOT)})).unwrap();
    s
}

fn request(s: &mut Session, folder: &Path, revision: &Value, source_bin: u64) -> Value {
    let mut args = json!({"folder":folder,"revision":revision,"sourceBin":source_bin,"bin":71});
    let preview = s.execute("project.previewExternalBin", args.clone()).unwrap();
    args["head"] = preview["head"].clone();
    args["epoch"] = preview["epoch"].clone();
    args
}

fn apply(s: &mut Session, args: Value) -> (BinId, ItemId, ItemId) {
    let result = s.execute("project.importExternalBin", args).unwrap();
    let id = BinId(result["bin"].as_u64().unwrap());
    let bin = s.project.root.find_bin(id).unwrap();
    let BinEntry::Item(cut) = bin.children[0] else { panic!("missing imported cut") };
    let source = s.project.sequence(cut).unwrap().video_tracks[0].items[0].item;
    (id, cut, source)
}

#[test]
fn independent_bin_imports_share_matching_sources_preserve_changed_copies_and_undo_as_a_unit() {
    let t = tempfile::tempdir().unwrap();
    let source = fixture(&t.path().join("external"), Path::new("offline"));
    let destination = fixture(&t.path().join("editor"), Path::new("offline"));
    let mut editor = open(&destination);
    let mut publisher = open(&source);
    let initial = publisher.execute("project.binHistory", json!({})).unwrap()["revisions"][0][1].clone();
    let a = request(&mut editor, &source, &initial, 70);
    let (first_bin, first_cut, shared) = apply(&mut editor, a);
    assert_ne!(first_cut, ItemId(21));
    assert_ne!(shared, ItemId(5));
    assert!(
        matches!(&editor.project.root.find_bin(first_bin).unwrap().children[1], BinEntry::Bin(b) if b.id != BinId(72) && b.name == "Empty" && b.children.is_empty())
    );
    editor.execute("file.save", json!({})).unwrap();
    drop(editor);
    let mut editor = open(&destination);
    let b = request(&mut editor, &source, &initial, 71);
    let (second_bin, second_cut, same) = apply(&mut editor, b.clone());
    assert_ne!(second_cut, first_cut);
    assert_eq!(same, shared);
    editor.execute("edit.undo", json!({})).unwrap();
    assert!(editor.project.root.find_bin(second_bin).is_none());
    assert!(editor.project.item(second_cut).is_none());
    assert!(editor.project.item(shared).is_some());
    editor.execute("edit.redo", json!({})).unwrap();
    assert_eq!(editor.project.sequence(second_cut).unwrap().video_tracks[0].items[0].item, shared);

    publisher.execute("metadata.set", json!({"item":5,"fields":{"Comments":"New take"}})).unwrap();
    publisher.execute("file.save", json!({})).unwrap();
    let before = editor.project.clone();
    assert!(editor.execute("project.importExternalBin", b).unwrap_err().to_string().contains("preview"));
    assert_eq!(editor.project, before);
    let latest = publisher.execute("project.binHistory", json!({})).unwrap()["revisions"][0][1].clone();
    let updated = request(&mut editor, &source, &latest, 71);
    let (_, _, changed) = apply(&mut editor, updated);
    assert_ne!(shared, changed);
    assert_eq!(editor.project.item(changed).unwrap().metadata["Comments"], "New take");
    assert_ne!(editor.project.item(shared).unwrap().metadata.get("Comments"), Some(&"New take".into()));

    editor.execute("metadata.set", json!({"item":shared,"fields":{"Comments":"Local edit"}})).unwrap();
    let historical = request(&mut editor, &source, &initial, 71);
    let (_, _, independent) = apply(&mut editor, historical);
    assert_ne!(independent, shared);
    assert_ne!(independent, changed);
    editor.execute("file.save", json!({})).unwrap();
    drop(editor);
    let mut editor = open(&destination);
    assert_eq!(editor.project.item(shared).unwrap().metadata["Comments"], "Local edit");
    let out = t.path().join("stock.fcproj");
    editor.execute("file.saveCopy", json!({"path":out})).unwrap();
    let bytes = fs::read(&out).unwrap();
    let stock = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(stock.sequence(first_cut).unwrap().video_tracks[0].items[0].item, shared);
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    let items = &doc["project"]["items"];
    assert_eq!(items[first_cut.0.to_string()]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["extension"]["slate"], 9);
    for (field, file) in [("media", "original.mov"), ("proxy", "proxy.mov")] {
        let path = items[shared.0.to_string()]["kind"]["Media"][field]["File"]["path"].as_str().unwrap();
        assert_eq!(craft_docpath::resolve_absolute(path, Some(&out)).unwrap(), t.path().join("external/offline").join(file));
    }
}

#[test]
fn import_rejects_a_busy_destination_and_scopes_source_identity_to_its_folder() {
    let t = tempfile::tempdir().unwrap();
    let first = fixture(&t.path().join("first"), &t.path().join("same-media"));
    let second = fixture(&t.path().join("second"), &t.path().join("same-media"));
    let dest = fixture(&t.path().join("dest"), &t.path().join("same-media"));
    let mut writer = open(&dest);
    let mut reader = open(&dest);
    let revision = reader.execute("project.binHistory", json!({})).unwrap()["revisions"][0][1].clone();
    // The identical starting files even share a revision digest. Neither their
    // numeric item IDs nor an equal revision alone grants destination identity.
    let first_request = request(&mut reader, &first, &revision, 70);
    let before = reader.project.clone();
    assert!(reader.execute("project.importExternalBin", first_request).unwrap_err().to_string().contains("read-only"));
    assert_eq!(reader.project, before);
    let a = request(&mut writer, &first, &revision, 70);
    let (_, _, a_source) = apply(&mut writer, a);
    let b = request(&mut writer, &second, &revision, 70);
    let (_, _, b_source) = apply(&mut writer, b);
    assert_ne!(a_source, b_source);
    let empty = request(&mut writer, &first, &revision, 72);
    let saved_manifest = fs::read(dest.join(ROOT)).unwrap();
    let id = writer.execute("project.importExternalBin", empty).unwrap()["bin"].as_u64().unwrap();
    let imported = writer.project.root.find_bin(BinId(id)).unwrap();
    assert_eq!(imported.name, "Empty");
    assert!(imported.children.is_empty());
    assert_eq!(fs::read(dest.join(ROOT)).unwrap(), saved_manifest);
}
