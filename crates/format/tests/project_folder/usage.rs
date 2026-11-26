use super::{manifest, payload, sample, snapshot, start};
use filmcraft_format::project_folder::{revisions::save, usage::find_uses};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tempfile::tempdir;

fn summary(folder: &Path, rev: &str, item: u64) -> Value {
    serde_json::to_value(find_uses(folder, rev, item).unwrap()).unwrap()
}

fn matches(report: &Value) -> Vec<(u64, bool)> {
    report["sequences"].as_array().unwrap().iter().map(|s| (s["id"].as_u64().unwrap(), s["direct"].as_bool().unwrap())).collect()
}

#[test]
fn saved_uses_follow_item_identity_across_cuts_subclips_and_secondary_filings() {
    let temp = tempdir().unwrap();
    let mut doc = sample();
    let p = &mut doc["project"];
    p["root"]["children"][0]["Bin"]["filed_sources"] = json!([5]);
    // 74 shares the source's name and media path and has an independent identity.
    p["items"]["74"]["kind"] = p["items"]["5"]["kind"].clone();
    let mut subclip = p["items"]["5"].clone();
    subclip["id"] = 80.into();
    subclip["kind"] = json!({"Subclip":{"parent":5,"range":{"start":0,"duration":100}}});
    p["items"]["80"] = subclip;
    let seq = &mut p["items"]["73"]["kind"]["Sequence"];
    seq["video_tracks"][0]["items"][0]["item"] = 21.into();
    seq["audio_tracks"][0]["items"][0]["item"] = 80.into();
    let (folder, first) = start(temp.path(), &doc);
    doc["project"]["items"]["73"]["kind"]["Sequence"]["video_tracks"][0]["items"][0]["item"] = 74.into();
    let edit = temp.path().join("edit.fcproj");
    fs::write(&edit, serde_json::to_vec(&doc).unwrap()).unwrap();
    let middle = save(&folder, &first, &edit).unwrap();
    doc["project"]["items"]["73"]["kind"]["Sequence"]["audio_tracks"][0]["items"][0]["item"] = 74.into();
    fs::write(&edit, serde_json::to_vec(&doc).unwrap()).unwrap();
    let last = save(&folder, &middle, &edit).unwrap();
    let before = snapshot(&folder);
    let report = summary(&folder, &first, 5);
    assert_eq!(report["complete"], true);
    assert_eq!(matches(&report), [(21, true), (73, false)]);
    let filed = report["filings"].as_array().unwrap();
    assert_eq!(filed.len(), 2);
    assert_eq!((filed[0]["bin"]["id"].as_u64(), &filed[0]["items"]), (Some(1), &json!([5, 21, 73])));
    assert_eq!(filed[1]["bin"]["path"], json!(["Legacy Edit", "Dailies"]));
    assert_eq!(filed[1]["items"], json!([5]));
    assert!(matches(&summary(&folder, &first, 74)).is_empty());
    assert_eq!(matches(&summary(&folder, &middle, 5)), [(21, true), (73, false)]);
    assert_eq!(matches(&summary(&folder, &last, 5)), [(21, true)]);
    assert_eq!(matches(&summary(&folder, &last, 74)), [(73, true)]);
    assert!(find_uses(&folder, &last, 123456).is_err());
    assert_eq!(before, snapshot(&folder));
}

#[test]
fn saved_uses_include_multicam_and_merged_inputs_without_timeline_clips() {
    let temp = tempdir().unwrap();
    let mut doc = sample();
    for id in ["21", "73"] {
        let s = &mut doc["project"]["items"][id]["kind"]["Sequence"];
        s["video_tracks"] = json!([]);
        s["audio_tracks"] = json!([]);
    }
    doc["project"]["items"]["21"]["kind"]["Sequence"]["multicam"] = json!({"cameras":[{"name":"A","video_track":null,"source":5}],"audio":"Camera1"});
    doc["project"]["items"]["73"]["kind"]["Sequence"]["merged"] = json!({"audio":[21,21]});
    let (folder, rev) = start(temp.path(), &doc);
    let report = summary(&folder, &rev, 5);
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(matches(&report), [(21, true), (73, false)]);
}

#[test]
fn damaged_saved_records_preserve_verified_uses_and_flag_incomplete_coverage() {
    let temp = tempdir().unwrap();
    let (folder, rev) = start(temp.path(), &sample());
    let mut m = manifest(&folder);
    for (table, id) in [("items", 73), ("bins", 3)] {
        fs::remove_file(folder.join(payload(&mut m, table, id).as_str().unwrap())).unwrap();
    }
    let before = snapshot(&folder);
    let report = summary(&folder, &rev, 5);
    assert_eq!(report["complete"], false);
    assert!(report["bin_errors"]["3"].is_string());
    assert!(report["item_errors"]["73"].is_string());
    assert_eq!(matches(&report), [(21, true)]);
    assert_eq!(report["filings"][0]["items"], json!([21]));
    let none = summary(&folder, &rev, 74);
    assert!(matches(&none).is_empty());
    assert_eq!(none["complete"], false);
    assert_eq!(before, snapshot(&folder));
}
