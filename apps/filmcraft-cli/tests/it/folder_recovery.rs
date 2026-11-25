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

fn kill_after_checkpoints(data: &Path, manifest: &Path, commands: &[(&str, Value)]) {
    let mut writer = super::filmcraft_command()
        .args(["--folder-recovery", "--data-dir"])
        .arg(data)
        .arg("--project")
        .arg(manifest)
        .args(["run", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = writer.stdin.take().unwrap();
    for (id, params) in commands {
        writeln!(input, "{}", json!({"id":id,"params":params})).unwrap();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout = writer.stdout.take().unwrap();
    let count = commands.len();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(count) {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut responses = Vec::new();
    for _ in commands {
        match rx.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(line) => responses.push(serde_json::from_str::<Value>(&line).unwrap()),
            Err(_) => break,
        }
    }
    writer.kill().ok();
    writer.wait().unwrap();
    assert!(responses.iter().all(|v| v["ok"] == true) && responses.len() == count, "{responses:?}");
    assert!(responses.last().unwrap()["result"]["checkpointRevision"].is_u64());
}

#[test]
fn killed_folder_writer_recovers_clip_in_fresh_cli_without_publishing_it() {
    crash_case(false);
}

#[test]
fn killed_writer_restores_saved_deletion_after_undo() {
    crash_case(true);
}

fn crash_case(undo_delete: bool) {
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
    let time = original["duration"].as_i64().unwrap();
    let seq = &mut doc["project"]["items"]["21"]["kind"]["Sequence"]["video_tracks"][0];
    let track_id = seq["id"].clone();
    let mut neighbor = original.clone();
    neighbor["id"] = 810001.into();
    neighbor["start"] = (time * 3).into();
    neighbor["extension"] = json!({"owner":"neighbor"});
    seq["items"].as_array_mut().unwrap().push(neighbor.clone());
    doc["project"]["next_id"] = 820000.into();
    let media = &mut doc["project"]["items"]["5"]["kind"]["Media"];
    media["media"] = json!({"File":{"path":"offline/shot.mov"}});
    media["proxy"] = json!({"File":{"path":"offline/shot-proxy.mov"}});
    fs::write(&stock, serde_json::to_vec(&doc).unwrap()).unwrap();
    filmcraft_format::project_folder::pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    let manual = fs::read(&manifest).unwrap();
    let mut commands = if undo_delete {
        vec![("timeline.delete", json!({"clip":original["id"]})), ("file.save", json!({})), ("edit.undo", json!({}))]
    } else {
        vec![("timeline.move", json!({"moves":[{"clip":original["id"],"track":track_id,"time":time}],"linked":false}))]
    };
    commands.push(("file.autoSaveNow", json!({})));
    kill_after_checkpoints(&data, &manifest, &commands);
    if !undo_delete {
        assert_eq!(fs::read(&manifest).unwrap(), manual);
    }
    let before = root.join("reader.fcproj");
    batch(&data, Some(&manifest), &[("file.saveCopy", json!({"path":before}))]);
    let read = |p: &Path| -> Value {
        let b = fs::read(p).unwrap();
        filmcraft_format::decode(&b).unwrap();
        serde_json::from_slice(&b).unwrap()
    };
    assert_eq!(read(&before).pointer(track).unwrap(), if undo_delete { &neighbor } else { &original });
    let list = batch(&data, None, &[("file.recoveryList", json!({}))]);
    assert_eq!(list[0].as_array().unwrap().len(), 1);
    let recovered = root.join("recovered.fcproj");
    batch(
        &data,
        None,
        &[
            ("file.recover", json!({"id":list[0][0]["id"]})),
            ("project.renameBin", json!({"bin":1,"name":"Recovered edit"})),
            ("file.saveCopy", json!({"path":recovered})),
            ("file.save", json!({})),
        ],
    );
    let mut moved = original;
    if !undo_delete {
        moved["start"] = time.into();
    }
    assert_eq!(read(&recovered).pointer(track).unwrap(), &moved);
    let reopened = root.join("reopened.fcproj");
    batch(&data, Some(&manifest), &[("file.saveCopy", json!({"path":reopened}))]);
    let checked = read(&reopened);
    assert_eq!(checked.pointer(track).unwrap(), &moved);
    assert_eq!(checked["project"]["items"]["21"]["kind"]["Sequence"]["video_tracks"][0]["items"][1], neighbor);
    for key in ["media", "proxy"] {
        assert_eq!(checked["project"]["items"]["5"]["kind"]["Media"][key], doc["project"]["items"]["5"]["kind"]["Media"][key]);
    }
}
