use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::mpsc::{Receiver, channel},
    time::Duration,
};

struct Editor {
    child: Child,
    input: ChildStdin,
    replies: Receiver<String>,
}
impl Editor {
    fn open(root: &Path, shared: bool) -> Self {
        let mut command = super::filmcraft_command();
        command.arg(if shared { "--shared-bins" } else { "--folder-recovery" });
        let mut child = command
            .arg("--data-dir")
            .arg(root.join("user"))
            .arg("--project")
            .arg(root.join("cut/project.fcmod"))
            .args(["--keep-going", "run", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, input, replies }
    }
    fn call(&mut self, id: &str, params: Value) -> Value {
        writeln!(self.input, "{}", json!({ "id":id, "params":params })).unwrap();
        self.input.flush().unwrap();
        serde_json::from_str(&self.replies.recv_timeout(Duration::from_secs(60)).unwrap()).unwrap()
    }
    fn ok(&mut self, id: &str, p: Value) -> Value {
        let response = self.call(id, p);
        assert_eq!(response["ok"], true, "{id}: {response}");
        response["result"].clone()
    }
    fn fails(&mut self, id: &str, p: Value, message: &str) {
        let response = self.call(id, p);
        assert!(response["ok"] == false && response.to_string().contains(message), "{id}: {response}");
    }
    fn checkout(&mut self, bin: u64) {
        assert_eq!(self.ok("project.checkoutBin", json!({"bin":bin}))["checkedOutBin"], bin);
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture(root: &Path) -> PathBuf {
    let stock = root.join("original.fcproj");
    let project = filmcraft_format::decode(include_bytes!("../../../../crates/format/tests/fixtures/v1-edit.fcproj")).unwrap().project;
    let mut doc: Value = serde_json::from_slice(&filmcraft_format::encode(&project, false)).unwrap();
    let p = &mut doc["project"];
    let clips = "/kind/Sequence/video_tracks/0/items";
    p["items"]["21"].pointer_mut(clips).unwrap()[0]["extension"] = json!({"camera":"A"});
    let mut second = p["items"]["21"].clone();
    second["id"] = 1021.into();
    second["kind"]["Sequence"]["markers"] = json!([]);
    second["name"] = "Cut B".into();
    for kind in ["video_tracks", "audio_tracks"] {
        for t in second["kind"]["Sequence"][kind].as_array_mut().unwrap() {
            t["id"] = (1000 + t["id"].as_u64().unwrap()).into();
            for c in t["items"].as_array_mut().unwrap() {
                c["id"] = (1000 + c["id"].as_u64().unwrap()).into();
            }
        }
    }
    second.pointer_mut(clips).unwrap()[0]["extension"] = json!({"camera":"B"});
    p["items"]["1021"] = second;
    p["items"]["5"]["unknownSourceField"] = json!([5, 4, 3]);
    p["root"]["children"] = json!([{"Bin":{"id":70,"name":"A","children":[{"Item":21},{"Bin":{"id":72,"name":"Empty","children":[]}}]}},{"Bin":{"id":71,"name":"B","children":[{"Item":1021}]}},{"Item":5}]);
    p["next_id"] = 2000.into();
    fs::write(&stock, serde_json::to_vec(&doc).unwrap()).unwrap();
    let folder = root.join("cut");
    filmcraft_format::project_folder::pack(&stock, &folder, filmcraft_format::atomic_write_new).unwrap();
    folder.join("project.fcmod")
}
fn published(root: &Path) -> Value {
    let temp = tempfile::tempdir_in(root).unwrap();
    let file = temp.path().join("read.fcproj");
    filmcraft_format::project_folder::unpack(&root.join("cut"), &file).unwrap();
    let bytes = fs::read(file).unwrap();
    filmcraft_format::decode(&bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn clips(doc: &Value, seq: u64) -> &Vec<Value> {
    doc["project"]["items"][seq.to_string()]["kind"]["Sequence"]["video_tracks"][0]["items"].as_array().unwrap()
}
#[test]
fn independent_processes_publish_both_orders_and_undo_only_owned_edits() {
    for reverse in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fixture(root);
        let mut a = Editor::open(root, true);
        let mut b = Editor::open(root, true);
        a.checkout(70);
        b.checkout(71);
        let mut observer = Editor::open(root, true);
        observer.fails("project.checkoutBin", json!({"bin":70}), "busy");
        let mut whole = Editor::open(root, false);
        assert_eq!(whole.ok("project.inspect", json!({}))["readOnly"], true);
        a.fails("project.checkoutBin", json!({"bin":71}), "busy");
        a.ok("sequence.open", json!({"item":1021}));
        a.fails("timeline.delete", json!({"clip":1029}), "outside");
        a.ok("sequence.open", json!({"item":21}));
        a.ok("project.reorderBin", json!({"bin":70,"order":[72,21]}));
        a.ok("project.renameBin", json!({"bin":70,"name":"Editorial"}));
        a.fails("project.releaseBin", json!({}), "discard");
        a.ok("timeline.razor", json!({"clip":29,"track":15,"time":500000000000_i64}));
        b.ok("timeline.razor", json!({"clip":1029,"track":1015,"time":500000000000_i64}));
        if reverse {
            b.ok("file.save", json!({}));
            a.ok("file.save", json!({}));
        } else {
            a.ok("file.save", json!({}));
            b.ok("file.save", json!({}));
        }
        let doc = published(root);
        assert_eq!(doc["project"]["root"]["children"][0]["Bin"]["children"][0]["Bin"]["id"], 72);
        assert_eq!(doc["project"]["items"]["5"]["unknownSourceField"], json!([5, 4, 3]));
        for (seq, owner) in [(21, "A"), (1021, "B")] {
            let v = clips(&doc, seq);
            assert_eq!(v.len(), 2);
            assert_eq!(v[0]["extension"]["camera"], owner);
            assert!(v.iter().all(|c| c["item"] == 5));
        }
        assert_ne!(clips(&doc, 21)[1]["id"], clips(&doc, 1021)[1]["id"]);
        a.ok("edit.undo", json!({}));
        a.ok("file.save", json!({}));
        let undone = published(root);
        assert_eq!(clips(&undone, 21).len(), 1);
        assert_eq!(clips(&undone, 1021), clips(&doc, 1021));
        a.ok("edit.redo", json!({}));
        a.ok("file.save", json!({}));
        assert_eq!(published(root)["project"]["items"], doc["project"]["items"]);
        let copy = root.join("snapshot.fcproj");
        a.ok("file.saveCopy", json!({"path":copy}));
        let pinned: Value = serde_json::from_slice(&fs::read(copy).unwrap()).unwrap();
        assert_eq!(clips(&pinned, 1021).len(), 1);
        a.ok("project.refresh", json!({}));
        let copy = root.join("refreshed.fcproj");
        a.ok("file.saveCopy", json!({"path":copy}));
        assert_eq!(clips(&serde_json::from_slice(&fs::read(copy).unwrap()).unwrap(), 1021).len(), 2);
        a.ok("project.releaseBin", json!({}));
        observer.checkout(70);
    }
}

#[test]
fn crashed_checkout_recovers_over_peer_save_but_rejects_changed_owner() {
    for stale in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let manifest = fixture(root);
        let mut a = Editor::open(root, true);
        let mut b = Editor::open(root, true);
        a.checkout(70);
        b.checkout(71);
        a.ok("timeline.delete", json!({"clip":29}));
        a.ok("file.autoSaveNow", json!({}));
        drop(a);
        b.ok("project.renameBin", json!({"bin":71,"name":"Peer published"}));
        b.ok("file.save", json!({}));
        let mut contender = Editor::open(root, true);
        contender.checkout(70);
        let mut recovery = Editor::open(root, true);
        let list = recovery.ok("file.recoveryList", json!({}));
        let id = &list[0]["id"];
        assert!(id.is_string(), "{list}");
        recovery.fails("file.recover", json!({"id":id}), "busy");
        if stale {
            contender.ok("project.renameBin", json!({"bin":70,"name":"Changed"}));
            contender.ok("file.save", json!({}));
            contender.ok("project.renameBin", json!({"bin":70,"name":"A"}));
            contender.ok("file.save", json!({}));
        }
        contender.ok("project.releaseBin", json!({}));
        let head = fs::read(&manifest).unwrap();
        if stale {
            recovery.fails("file.recover", json!({"id":id}), "changed since checkout");
            let copy = root.join("rescue.fcproj");
            fs::write(&copy, b"occupied").unwrap();
            recovery.fails("file.recover", json!({"id":id,"path":copy}), "exist");
            assert_eq!(fs::read(&copy).unwrap(), b"occupied");
            fs::remove_file(&copy).unwrap();
            recovery.ok("file.recover", json!({"id":id,"path":copy}));
            assert!(clips(&serde_json::from_slice(&fs::read(copy).unwrap()).unwrap(), 21).is_empty());
            assert_eq!(fs::read(manifest).unwrap(), head);
        } else {
            recovery.ok("file.recover", json!({"id":id}));
            assert_eq!(fs::read(&manifest).unwrap(), head);
            recovery.ok("file.save", json!({}));
            let result = published(root);
            assert!(clips(&result, 21).is_empty());
            assert_eq!(result["project"]["root"]["children"][1]["Bin"]["name"], "Peer published");
        }
    }
}

#[test]
fn scoped_gestures_discard_and_simultaneous_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let manifest = fixture(root);
    let mut whole = Editor::open(root, false);
    assert_eq!(whole.ok("project.inspect", json!({}))["readOnly"], false);
    let mut a = Editor::open(root, true);
    a.fails("project.checkoutBin", json!({"bin":70}), "busy");
    drop(whole);
    a.fails("project.checkoutBin", json!({"bin":0}), "non-root");
    a.checkout(70);
    a.fails("project.renameBin", json!({"bin":72,"name":"No"}), "outside");
    a.fails("project.reorderBin", json!({"bin":70,"order":[21,21]}), "repeated");
    a.fails("file.newBin", json!({"name":"No"}), "unavailable");
    let before = fs::read(&manifest).unwrap();
    a.ok("timeline.trim", json!({"clip":29,"edge":"out","deltaFrames":-4,"linked":false}));
    a.ok("timeline.move", json!({"moves":[{"clip":29,"track":15,"time":254016000000_i64}],"linked":false}));
    a.ok("timeline.place", json!({"item":5,"frame":250,"track":"V1","video":true,"audio":false,"insert":true}));
    a.ok("file.autoSaveNow", json!({}));
    a.fails("project.refresh", json!({}), "save");
    a.ok("project.releaseBin", json!({"discard":true}));
    assert_eq!(fs::read(&manifest).unwrap(), before);
    assert_eq!(a.ok("file.recoveryList", json!({})), json!([]));
    a.checkout(70);
    a.fails("edit.undo", json!({}), "nothing to undo");
    let mut b = Editor::open(root, true);
    b.checkout(71);
    a.ok("timeline.trim", json!({"clip":29,"edge":"out","deltaFrames":-4,"linked":false}));
    b.ok("timeline.move", json!({"moves":[{"clip":1029,"track":1015,"time":254016000000_i64}],"linked":false}));
    let go = std::sync::Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            go.wait();
            a.ok("file.save", json!({}));
        });
        go.wait();
        b.ok("file.save", json!({}));
    });
    let saved = published(root);
    assert!(clips(&saved, 21)[0]["duration"].as_i64().unwrap() < 1260755496000);
    assert!(clips(&saved, 1021)[0]["start"].as_i64().unwrap() > 0);
}

#[test]
fn crash_draft_can_export_if_a_whole_project_editor_reused_an_unpublished_id() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let manifest = fixture(root);
    let mut a = Editor::open(root, true);
    a.checkout(70);
    a.ok("timeline.razor", json!({"clip":29,"track":15,"time":500000000000_i64}));
    a.ok("file.autoSaveNow", json!({}));
    drop(a);
    let mut whole = Editor::open(root, false);
    whole.ok("sequence.open", json!({"item":1021}));
    whole.ok("timeline.razor", json!({"clip":1029,"track":1015,"time":500000000000_i64}));
    whole.ok("file.save", json!({}));
    drop(whole);
    let before = fs::read(&manifest).unwrap();
    let mut recovery = Editor::open(root, true);
    let list = recovery.ok("file.recoveryList", json!({}));
    let id = &list[0]["id"];
    recovery.fails("file.recover", json!({"id":id}), "reserved IDs overlap");
    let dest = root.join("independent.fcproj");
    recovery.ok("file.recover", json!({"id":id,"path":dest}));
    assert_eq!(fs::read(manifest).unwrap(), before);
    let separate: Value = serde_json::from_slice(&fs::read(dest).unwrap()).unwrap();
    assert_eq!(clips(&separate, 21).len(), 2);
    assert_eq!(clips(&separate, 1021).len(), 1);
    let live = published(root);
    assert_eq!(clips(&live, 21).len(), 1);
    assert_eq!(clips(&live, 1021).len(), 2);
}

#[test]
fn partial_checkout_releases_all_locks_and_concurrent_nesting_refuses_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let path = fixture(root);
    let dir = path.parent().unwrap();
    let lock = fs::File::create(dir.join(".shared-sequence-21.lock")).unwrap();
    lock.try_lock().unwrap();
    let mut a = Editor::open(root, true);
    a.fails("project.checkoutBin", json!({"bin":70}), "sequence 21 is busy");
    let mut whole = Editor::open(root, false);
    assert_eq!(whole.ok("project.inspect", json!({}))["readOnly"], false);
    drop(whole);
    drop(lock);
    a.checkout(70);
    let mut b = Editor::open(root, true);
    b.checkout(71);
    for (writer, nested) in [(&mut a, 1021), (&mut b, 21)] {
        writer.ok("timeline.place", json!({"item":nested,"frame":500,"video":true,"audio":false}));
    }
    a.ok("file.save", json!({}));
    let before = fs::read(&path).unwrap();
    b.fails("file.save", json!({}), "cycle");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(b.ok("project.inspect", json!({}))["dirty"], true);
    b.ok("edit.undo", json!({}));
    b.ok("file.save", json!({}));
    let out = published(root);
    assert!(clips(&out, 21).iter().any(|c| c["item"] == 1021));
    assert!(clips(&out, 1021).iter().all(|c| c["item"] == 5));
}
