use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::Stdio,
};

fn batch(data: &Path, project: Option<&Path>, script: &[(&str, Value)]) -> Vec<Value> {
    let mut cmd = super::filmcraft_command();
    cmd.args(["--folder-recovery", "--data-dir"]).arg(data);
    if let Some(path) = project {
        cmd.arg("--project").arg(path);
    }
    let mut child = cmd.args(["run", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    for (id, params) in script {
        writeln!(input, "{}", json!({ "id":id,"params":params })).unwrap();
    }
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["result"].clone()).collect()
}

#[test]
fn killed_folder_writer_recovers_clip_in_fresh_cli_without_publishing_it() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let data = root.join("user");
    let stock = root.join("source.fcproj");
    let folder = root.join("cut");
    let manifest = folder.join("project.fcmod");
    let p = filmcraft_format::decode(include_bytes!("../../../../crates/format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&p, false)).unwrap();
    let track = "/project/items/21/kind/Sequence/video_tracks/0/items/0";
    let clip = doc.pointer_mut(track).unwrap();
    clip["extension"] = json!({"nested":[null,{"camera":4}]});
    let original = clip.clone();
    fs::write(&stock, serde_json::to_vec(&doc).unwrap()).unwrap();
    filmcraft_format::project_folder::pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let manual = fs::read(&manifest).unwrap();
    let mut writer = super::filmcraft_command()
        .args(["--folder-recovery", "--data-dir"])
        .arg(&data)
        .arg("--project")
        .arg(&manifest)
        .args(["run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = writer.stdin.take().unwrap();
    let track_id = &doc["project"]["items"]["21"]["kind"]["Sequence"]["video_tracks"][0]["id"];
    let time = original["duration"].as_i64().unwrap();
    for (id, params) in
        [("timeline.move", json!({"moves":[{"clip":original["id"],"track":track_id,"time":time}],"linked":false})), ("file.autoSaveNow", json!({}))]
    {
        writeln!(input, "{}", json!({"id":id,"params":params})).unwrap();
    }
    // A bounded acknowledgement from the actual command stream, then kill without shutdown.
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout = writer.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(2) {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let ack = (0..2).map(|_| rx.recv_timeout(std::time::Duration::from_secs(20))).collect::<Result<Vec<_>, _>>();
    writer.kill().ok();
    writer.wait().unwrap();
    drop(input);
    for line in ack.expect("writer did not acknowledge checkpoint before termination") {
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["ok"], true, "{line}");
    }
    assert_eq!(fs::read(&manifest).unwrap(), manual);
    let before = root.join("reader.fcproj");
    batch(&data, Some(&manifest), &[("file.saveCopy", json!({"path":before}))]);
    let read = |p: &Path| -> Value {
        let b = fs::read(p).unwrap();
        filmcraft_format::decode(&b).unwrap();
        serde_json::from_slice(&b).unwrap()
    };
    assert_eq!(read(&before).pointer(track).unwrap(), &original);
    let list = batch(&data, None, &[("file.recoveryList", json!({}))]);
    assert_eq!(list[0].as_array().unwrap().len(), 1);
    let recovered = root.join("recovered.fcproj");
    batch(&data, None, &[("file.recover", json!({"id":list[0][0]["id"]})), ("file.saveCopy", json!({"path":recovered})), ("file.save", json!({}))]);
    let mut moved = original;
    moved["start"] = time.into();
    assert_eq!(read(&recovered).pointer(track).unwrap(), &moved);
    let reopened = root.join("reopened.fcproj");
    batch(&data, Some(&manifest), &[("file.saveCopy", json!({"path":reopened}))]);
    assert_eq!(read(&reopened).pointer(track).unwrap(), &moved);
}
