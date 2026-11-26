use super::{bin, manifest, payload, sample, snapshot, start};
use crate::project_folder::{save, write_doc};
use filmcraft_format::project_folder::compare::{Selection, compare};
use serde_json::{Value, json};
use std::{fs, path::Path};
use tempfile::tempdir;

fn publish(folder: &Path, base: &str, doc: &Value) -> String {
    let input = folder.with_file_name("edited.fcproj");
    write_doc(&input, doc);
    save(folder, base, &input).unwrap()
}

fn report(folder: &Path, from: &str, to: &str, selection: Selection, complete: bool) -> Vec<Value> {
    let mut buf = Vec::new();
    let result = compare(folder, from, to, selection, &mut buf);
    let mut rows = serde_json::Deserializer::from_slice(&buf).into_iter::<Value>().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(result.is_ok(), complete, "{result:?} {rows:?}");
    let end = rows.pop().unwrap();
    assert_eq!(end["complete"], complete);
    assert_eq!((end["from"].as_str(), end["to"].as_str()), (Some(from), Some(to)));
    rows
}

#[test]
fn bin_membership_is_distinct_from_identity_and_relative_order() {
    let t = tempdir().unwrap();
    let mut doc = sample();
    let (folder, a) = start(t.path(), &doc);
    let members = doc.pointer_mut("/project/root/children/0/Bin/children").unwrap().as_array_mut().unwrap();
    members.insert(0, bin(91, "Dailies", json!([])));
    let b = publish(&folder, &a, &doc);
    let sel = Selection::Bin(1);
    let first = report(&folder, &a, &b, sel, true);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["record"], json!({"Bin":91}));
    assert_eq!(first[0]["changes"], json!(["added"]));
    let members = doc.pointer_mut("/project/root/children/0/Bin/children").unwrap().as_array_mut().unwrap();
    members.swap(0, 2);
    members[0]["Bin"]["name"] = json!("Renamed");
    let moved = members.pop().unwrap();
    doc["project"]["root"]["children"][1]["Bin"]["children"].as_array_mut().unwrap().push(moved);
    let c = publish(&folder, &b, &doc);
    let rows = report(&folder, &b, &c, sel, true);
    assert!(rows.iter().any(|r| r["change"] == "reordered"
        && r["before"] == json!([{"Bin":91},{"Item":21},{"Bin":2}])
        && r["after"] == json!([{"Bin":2},{"Item":21},{"Bin":91}])));
    assert!(rows.iter().any(|r| r["record"] == json!({"Bin":2}) && r["changes"] == json!(["renamed"])));
    assert!(rows.iter().any(|r| r["record"] == json!({"Item":73}) && r["changes"] == json!(["removed"]) && r["absentFromProject"] == false));
    let all = report(&folder, &b, &c, Selection::Bin(0), true);
    assert!(all.iter().any(|r| r["changes"] == json!(["moved"]) && r["beforeParent"] == 1 && r["afterParent"] == 3));
    assert!(report(&folder, &c, &c, sel, true).is_empty());
}

#[test]
fn sequence_reports_edit_coordinates_in_each_revision_and_reverses_direction() {
    let t = tempdir().unwrap();
    let mut doc = sample();
    let path = "/project/items/21/kind/Sequence";
    let seq = doc.pointer_mut(path).unwrap();
    seq["start_timecode"] = json!(24);
    let mut second = seq["video_tracks"][0].clone();
    second["id"] = json!(490);
    second["items"] = json!([]);
    seq["video_tracks"].as_array_mut().unwrap().push(second);
    let (folder, a) = start(t.path(), &doc);
    let seq = doc.pointer_mut(path).unwrap();
    let mut clip = seq["video_tracks"][0]["items"].as_array_mut().unwrap().remove(0);
    let old_start = clip["start"].as_i64().unwrap();
    clip["start"] = json!(254016000000_i64);
    clip["duration"] = json!(clip["duration"].as_i64().unwrap() - 1);
    clip["item"] = json!(74);
    clip["private-field"] = json!(true);
    seq["video_tracks"][1]["items"] = json!([clip]);
    let mut additional = clip.clone();
    additional["id"] = json!(499);
    additional["start"] = json!(4_000_000_000_000_i64);
    seq["video_tracks"][0]["items"] = json!([additional]);
    seq["audio_tracks"][0]["items"] = json!([]);
    seq["settings"]["frame_rate"] = json!({"num":25,"den":1});
    seq["start_timecode"] = json!(50);
    let b = publish(&folder, &a, &doc);
    let sel = Selection::Sequence(21);
    let rows = report(&folder, &a, &b, sel, true);
    let rev = report(&folder, &b, &a, sel, true);
    let edited = rows.iter().find(|r| r["clip"] == 29).unwrap();
    assert_eq!(edited["changes"], json!(["trackMoved", "timelineMoved", "trimmed", "sourceReplaced", "other"]));
    assert_eq!(edited["before"]["start"], old_start);
    assert_eq!(edited["after"]["startTimecode"], "00:00:03:00");
    assert_eq!(edited["before"]["source"], 5);
    assert_eq!(edited["after"]["source"], 74);
    let reverse = rev.iter().find(|r| r["clip"] == 29).unwrap();
    assert_eq!(reverse["before"], edited["after"]);
    assert_eq!(reverse["after"], edited["before"]);
    for (id, forward, back) in [(30, "removed", "added"), (499, "added", "removed")] {
        assert!(rows.iter().any(|r| r["clip"] == id && r["changes"] == json!([forward])));
        assert!(rev.iter().any(|r| r["clip"] == id && r["changes"] == json!([back])));
    }
}

#[test]
fn source_relink_slip_and_opaque_changes_are_distinct() {
    let t = tempdir().unwrap();
    let mut doc = sample();
    let (folder, a) = start(t.path(), &doc);
    let sel = Selection::Sequence(21);
    doc["project"]["items"]["5"]["kind"]["Media"]["media"] = json!({"File":{"path":"other.mov"}});
    let b = publish(&folder, &a, &doc);
    let linked = report(&folder, &a, &b, sel, true);
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0]["scope"], "dependency");
    assert_eq!(linked[0]["record"], json!({"Item":5}));
    let clip = "/project/items/21/kind/Sequence/video_tracks/0/items/0";
    let c = doc.pointer_mut(clip).unwrap();
    c["source_in"] = json!(c["source_in"].as_i64().unwrap() + 1);
    let next = publish(&folder, &b, &doc);
    let slips = report(&folder, &b, &next, sel, true);
    assert_eq!(slips.len(), 1);
    assert_eq!(slips[0]["changes"], json!(["sourceInChanged"]));
    assert_eq!(slips[0]["before"]["start"], slips[0]["after"]["start"]);
    doc.pointer_mut(clip).unwrap()["opaque"] = json!({"v":3});
    let d = publish(&folder, &next, &doc);
    let unknown = report(&folder, &next, &d, sel, true);
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0]["changes"], json!(["other"]));
    let summary = report(&folder, &next, &d, Selection::Bin(1), true);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0]["changes"], json!(["sequenceChanged"]));
    assert_eq!(summary[0]["record"], json!({"Item":21}));
}

#[test]
fn damage_preserves_independent_findings_and_never_implies_deletion() {
    let t = tempdir().unwrap();
    let mut doc = sample();
    let (folder, a) = start(t.path(), &doc);
    doc["project"]["items"]["21"]["name"] = json!("Fine cut");
    doc["project"]["root"]["children"][0]["Bin"]["children"].as_array_mut().unwrap().swap(0, 2);
    let b = publish(&folder, &a, &doc);
    let mut m = manifest(&folder);
    let sel = Selection::Bin(1);
    fs::remove_file(folder.join(payload(&mut m, "bins", 3).as_str().unwrap())).unwrap();
    assert_eq!(report(&folder, &a, &b, sel, true).len(), 2);
    for (group, id) in [("bins", 2), ("items", 5), ("items", 21)] {
        fs::remove_file(folder.join(payload(&mut m, group, id).as_str().unwrap())).unwrap();
        let before = snapshot(&folder);
        let rows = report(&folder, &a, &b, sel, false);
        assert!(rows.iter().any(|r| r["change"] == "reordered"));
        assert_eq!(snapshot(&folder), before);
        let key = if group == "bins" { json!({"Bin":id}) } else { json!({"Item":id}) };
        assert!(rows.iter().any(|r| r["record"] == key && r["revision"] == b && r["error"].is_string()));
        assert!(!rows.iter().any(|r| r["changes"].as_array().is_some_and(|k| k.contains(&json!("removed")))));
        if id != 21 {
            assert!(rows.iter().any(|r| r["afterName"] == "Fine cut"));
        }
    }
}
