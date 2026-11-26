use super::converter::bins::{list_bins, rescue_bin};
use super::{ROOT, checkout, current, pack, save, write_doc};
use filmcraft_format::atomic_write_new;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};
use tempfile::tempdir;

fn bin(id: u64, name: &str, children: Value) -> Value {
    json!({"Bin":{"id":id,"name":name,"children":children}})
}

fn sample() -> Value {
    let mut doc = current();
    let p = &mut doc["project"];
    p["root"]["children"] =
        json!([bin(1, "Referenced Items", json!([{"Item":21},bin(2,"Dailies",json!([])),{"Item":73}])), bin(3, "Dailies", json!([{"Item":5}]))]);
    p["items"]["5"]["kind"]["Media"]["media"] = json!({"File":{"path":"media/offline.mov"}});
    p["items"]["5"]["kind"]["Media"]["proxy"] = json!({"File":{"path":"proxy/offline.mov"}});
    p["items"]["73"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["item"] = json!(5);
    for (field, value) in [
        ("transcripts", json!({"language":"en","source":"manual","speakers":[],"words":[]})),
        ("source_graphics", json!({"layers":[]})),
        ("narrations", json!({"text":"Pickup","language":"en","voice":"local","pace":1.0,"speechDuration":100})),
    ] {
        p[field] = json!({"5":value,"74":value});
    }
    p["luts"] = json!([{"id":"lut999","name":"Look","format":"cube","text":"TITLE test"}]);
    doc
}

fn start(dir: &Path, doc: &Value) -> (std::path::PathBuf, String) {
    let folder = dir.join("folder");
    write_doc(&dir.join("input"), doc);
    pack(&dir.join("input"), &folder, atomic_write_new).unwrap();
    let rev = checkout(&folder, &dir.join("checkout")).unwrap();
    (folder, rev)
}

fn listing(folder: &Path, revision: &str, complete: bool) -> BTreeMap<u64, Value> {
    let mut buf = Vec::new();
    assert_eq!(list_bins(folder, revision, &mut buf).is_ok(), complete);
    serde_json::Deserializer::from_slice(&buf)
        .into_iter::<Value>()
        .map(|row| {
            let row = row.unwrap();
            (row["id"].as_u64().unwrap(), row)
        })
        .collect()
}

fn manifest(folder: &Path) -> Value {
    serde_json::from_slice(&fs::read(folder.join(ROOT)).unwrap()).unwrap()
}
fn payload<'a>(manifest: &'a mut Value, group: &str, id: u64) -> &'a mut Value {
    &mut manifest[group].as_array_mut().unwrap().iter_mut().find(|row| row[0] == id).unwrap()[1]
}
fn snapshot(folder: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
    fs::read_dir(folder)
        .unwrap()
        .map(|p| {
            let p = p.unwrap();
            (p.file_name(), fs::read(p.path()).unwrap())
        })
        .collect()
}

#[test]
fn choose_a_saved_bin_after_rename_or_deletion_even_with_an_unreadable_ancestor() {
    let tmp = tempdir().unwrap();
    let mut doc = sample();
    let (folder, old) = start(tmp.path(), &doc);
    let rows = listing(&folder, &old, true);
    assert_eq!(rows[&2]["path"], json!(["Legacy Edit", "Referenced Items", "Dailies"]));
    assert_eq!(rows[&3]["path"], json!(["Legacy Edit", "Dailies"]));
    doc["project"]["root"]["children"][0]["Bin"]["name"] = json!("Renamed");
    doc["project"]["root"]["children"].as_array_mut().unwrap().pop();
    write_doc(&tmp.path().join("edited"), &doc);
    let head = save(&folder, &old, &tmp.path().join("edited")).unwrap();
    let current = listing(&folder, &head, true);
    assert!(!current.contains_key(&3));
    assert_eq!(current[&2]["path"][1], "Renamed");
    assert_eq!(listing(&folder, &old, true), rows);
    rescue_bin(&folder, &old, 3, &tmp.path().join("deleted.fcproj")).unwrap();
    let mut archived: Value = serde_json::from_slice(&fs::read(folder.join(&old)).unwrap()).unwrap();
    fs::remove_file(folder.join(payload(&mut archived, "bins", 1).as_str().unwrap())).unwrap();
    let incomplete = listing(&folder, &old, false);
    assert!(incomplete[&1]["error"].as_str().unwrap().contains("bin 1"));
    assert_eq!(incomplete[&2]["name"], "Dailies");
    assert!(incomplete[&2]["path"].is_null());
    assert_eq!(incomplete[&3], rows[&3]);
    let out = tmp.path().join("empty.fcproj");
    rescue_bin(&folder, &old, 2, &out).unwrap();
    let result: Value = serde_json::from_slice(&fs::read(out).unwrap()).unwrap();
    assert_eq!(result["project"]["root"]["children"], json!([bin(2, "Dailies", json!([]))]));
}

#[test]
fn bin_export_preserves_order_shared_sources_offline_paths_and_the_source_folder() {
    let tmp = tempdir().unwrap();
    let doc = sample();
    let (folder, base) = start(tmp.path(), &doc);
    let dest = tmp.path().join("delivery");
    fs::create_dir(&dest).unwrap();
    let export = dest.join("Recovered.fcproj");
    let as_root = dest.join("Root.fcproj");
    rescue_bin(&folder, &base, 0, &as_root).unwrap();
    let all: Value = serde_json::from_slice(&fs::read(as_root).unwrap()).unwrap();
    assert_eq!(all["project"]["root"], doc["project"]["root"]);
    let mut m = manifest(&folder);
    fs::remove_file(folder.join(payload(&mut m, "bins", 3).as_str().unwrap())).unwrap();
    let before = snapshot(&folder);
    rescue_bin(&folder, &base, 1, &export).unwrap();
    let bytes = fs::read(&export).unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    let project = &result["project"];
    let children = project["root"]["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0], doc["project"]["root"]["children"][0]);
    assert_eq!(children[1]["Bin"]["children"], json!([{"Item":5}]));
    assert_eq!(children[1]["Bin"]["name"], "Referenced Items 2");
    assert!(children[1]["Bin"]["id"].as_u64().unwrap() >= doc["project"]["next_id"].as_u64().unwrap());
    assert_eq!(project["items"].as_object().unwrap().len(), 3);
    assert_eq!(project["luts"], doc["project"]["luts"]);
    for field in ["transcripts", "source_graphics", "narrations"] {
        assert_eq!(project[field], json!({"5":doc["project"][field]["5"]}));
    }
    for (field, original) in [("media", "media/offline.mov"), ("proxy", "proxy/offline.mov")] {
        let stored = project["items"]["5"]["kind"]["Media"][field]["File"]["path"].as_str().unwrap();
        assert_eq!(craft_docpath::resolve_absolute(stored, Some(&export)).unwrap(), tmp.path().join(original));
    }
    for seq in ["21", "73"] {
        assert_eq!(project["items"][seq], doc["project"]["items"][seq]);
    }
    for field in ["media", "proxy"] {
        let source = &doc["project"]["items"]["5"]["kind"]["Media"][field]["File"]["path"];
        let rescued = &project["items"]["5"]["kind"]["Media"][field]["File"]["path"];
        assert_eq!(
            craft_docpath::resolve_absolute(source.as_str().unwrap(), Some(&tmp.path().join("input"))).unwrap(),
            craft_docpath::resolve_absolute(rescued.as_str().unwrap(), Some(&export)).unwrap()
        );
    }
    assert!(rescue_bin(&folder, &base, 1, &export).is_err());
    assert_eq!(fs::read(export).unwrap(), bytes);
    assert_eq!(snapshot(&folder), before);
}

#[test]
fn required_damage_or_a_bin_cycle_refuses_publication() {
    let tmp = tempdir().unwrap();
    let (folder, base) = start(tmp.path(), &sample());
    let mut m = manifest(&folder);
    let out = tmp.path().join("out.fcproj");
    let src = folder.join(payload(&mut m, "items", 5).as_str().unwrap());
    fs::remove_file(src).unwrap();
    let before = snapshot(&folder);
    assert!(rescue_bin(&folder, &base, 1, &out).unwrap_err().to_string().contains("item 5"));
    assert!(!out.exists());
    assert_eq!(snapshot(&folder), before);
    let cyclic = json!({"id":2,"name":"Cyclic","children":[{"Bin":1}]});
    let encoded = serde_json::to_vec(&cyclic).unwrap();
    let hash = format!("{:x}", Sha256::digest(&encoded));
    fs::write(folder.join(&hash), encoded).unwrap();
    *payload(&mut m, "bins", 2) = json!(hash);
    let root = serde_json::to_vec(&m).unwrap();
    let corrupt = format!("{:x}", Sha256::digest(&root));
    fs::write(folder.join(ROOT), root).unwrap();
    let before = snapshot(&folder);
    assert!(rescue_bin(&folder, &corrupt, 1, &out).unwrap_err().to_string().contains("hierarchy at bin 1"));
    assert!(listing(&folder, &corrupt, false)[&1]["path"].is_null());
    assert!(!out.exists());
    assert_eq!(snapshot(&folder), before);
}

#[path = "compare.rs"]
mod comparison;

#[test]
fn paste_recovers_used_lut_and_sidecars_without_replacing_the_live_grade() {
    let t = tempdir().unwrap();
    let mut historical = sample();
    let fx = &mut historical["project"]["items"]["21"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["effects"][0];
    fx["params"]["input_lut"] = json!({"value":{"Text":"lib:lut999"}});
    let (folder, rev) = start(t.path(), &historical);
    let mut current: Value = serde_json::from_slice(&fs::read(t.path().join("checkout")).unwrap()).unwrap();
    current["project"]["luts"][0]["text"] = "different grade".into();
    current["project"]["items"]["5"]["metadata"]["Comments"] = "current".into();
    let ids_before = current["project"]["items"].as_object().unwrap().len();
    let paste = filmcraft_format::project_folder::history_paste::prepare(&folder, &rev, &[21, 73], 3, &serde_json::to_vec(&current).unwrap(), 10000).unwrap();
    let doc: Value = serde_json::from_slice(&paste.document).unwrap();
    let p = &doc["project"];
    let copies = &p["items"];
    assert_eq!(copies.as_object().unwrap().len(), ids_before + 3);
    let clip = |i: usize| &copies[paste.selected[i].to_string()]["kind"]["Sequence"]["video_tracks"][0]["items"][0];
    let source = clip(0)["item"].as_u64().unwrap().to_string();
    assert_ne!(source, "5");
    assert_eq!(clip(0)["item"], clip(1)["item"]);
    for field in ["transcripts", "source_graphics", "narrations"] {
        assert_eq!(p[field][&source], p[field]["5"]);
    }
    let grade = clip(0)["effects"][0]["params"]["input_lut"]["value"]["Text"].as_str().unwrap().strip_prefix("lib:").unwrap();
    assert_ne!(grade, "lut999");
    let luts = p["luts"].as_array().unwrap();
    assert!(luts.iter().any(|l| l["id"] == grade && l["text"] == "TITLE test"));
    assert!(luts.iter().any(|l| l["id"] == "lut999" && l["text"] == "different grade"));
}

#[path = "usage.rs"]
mod usage;
