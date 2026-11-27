//! Plan against immutable history, then validate the head and catalog before each
//! short mutation. A new save, pin, or policy change invalidates stale work.
use super::{Catalog, MANIFEST_LIMIT, plan, publish_visibility};
use crate::project_folder::{ROOT, digest, editor::FileStamp, invalid, read_member, revisions};
use std::{
    fs, io,
    path::Path,
    sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(in crate::project_folder) struct Maintenance {
    worker: Option<(SyncSender<Request>, Receiver<String>)>,
}

enum Request {
    Run,
    Barrier(Sender<Result<(), String>>),
}

impl Maintenance {
    pub(in crate::project_folder) fn schedule(&mut self, folder: &Path, total: u64) -> io::Result<()> {
        if !Catalog::read(folder)?.due(total) {
            return Ok(());
        }
        if self.worker.is_none() {
            let (tx, requests) = mpsc::sync_channel(1);
            let (errors, rx) = mpsc::channel();
            let dir = folder.to_owned();
            thread::Builder::new().name("filmcraft-bin-retention".into()).spawn(move || {
                let mut result = Ok(());
                for request in requests {
                    match request {
                        Request::Run => {
                            result = collect(&dir).map_err(|e| e.to_string());
                            if let Err(e) = &result {
                                log::warn!("bin backup cleanup: {e}");
                                let _ = errors.send(e.clone());
                            }
                        }
                        Request::Barrier(done) => {
                            let _ = done.send(result.clone());
                        }
                    }
                }
            })?;
            self.worker = Some((tx, rx));
        }
        if let Some((tx, _)) = &self.worker {
            match tx.try_send(Request::Run) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    self.worker = None;
                    return Err(io::Error::other("bin backup cleanup worker stopped"));
                }
            }
        }
        Ok(())
    }

    pub(in crate::project_folder) fn warning(&self) -> Option<String> {
        self.worker.as_ref()?.1.try_recv().ok()
    }

    pub(in crate::project_folder) fn wait(&self) -> io::Result<()> {
        if let Some((requests, _)) = &self.worker {
            let (tx, rx) = mpsc::channel();
            requests.send(Request::Barrier(tx)).map_err(io::Error::other)?;
            rx.recv().map_err(io::Error::other)?.map_err(io::Error::other)?;
        }
        Ok(())
    }
}

fn current(folder: &Path, revision: &str, catalog: &Catalog) -> io::Result<bool> {
    Ok(digest(&read_member(&folder.join(ROOT), MANIFEST_LIMIT)?) == revision && Catalog::read(folder)? == *catalog)
}

fn transaction(folder: &Path, revision: &str, catalog: &Catalog, action: impl FnOnce() -> io::Result<()>) -> io::Result<bool> {
    let start = Instant::now();
    let _locks = loop {
        match revisions::maintenance_writer(folder) {
            Ok(locks) => break locks,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && start.elapsed() < Duration::from_secs(5) => thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e),
        }
    };
    if !current(folder, revision, catalog)? {
        return Ok(false);
    }
    action()?;
    Ok(true)
}

fn collect(folder: &Path) -> io::Result<()> {
    let _worker = revisions::retention_worker(folder)?;
    loop {
        let bytes = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
        let head: super::Manifest = serde_json::from_slice(&bytes)?;
        let state = Catalog::read(folder)?;
        if !state.due(head.revision.as_ref().map_or(0, |r| r.total_bin_backups)) {
            return Ok(());
        }
        let revision = digest(&bytes);
        // Give up an obsolete scan while there are still gigabytes of retained
        // history left to verify. The locked comparison remains authoritative.
        let mut checked = Instant::now();
        let check_progress = || {
            if checked.elapsed() >= Duration::from_millis(500) {
                if !current(folder, &revision, &state)? {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "project or retention policy changed"));
                }
                checked = Instant::now();
            }
            Ok(())
        };
        let planned = match plan(folder, &state, state.policy, &revision, None, check_progress) {
            Ok(plan) => plan,
            Err(_) if !current(folder, &revision, &state)? => continue,
            Err(e) => return Err(e),
        };
        let mut published = state.clone();
        if !transaction(folder, &revision, &state, || publish_visibility(folder, &revision, &mut published, &planned))? {
            continue;
        }
        if planned.garbage.is_empty() {
            return Ok(());
        }
        if collect_garbage(folder, &revision, &published, &planned.garbage)? {
            return Ok(());
        }
    }
}

fn collect_garbage(folder: &Path, revision: &str, state: &Catalog, garbage: &[String]) -> io::Result<bool> {
    // Limit unlink work per foreground admission window. Read and hash all
    // candidate bytes outside the writer lock. Recheck their identity inside.
    for batch in garbage.chunks(16) {
        let mut verified = Vec::with_capacity(batch.len());
        for hash in batch {
            let path = folder.join(hash);
            let stamp = match FileStamp::read(&path) {
                Ok(stamp) => stamp,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            match read_member(&path, crate::MAX_PROJECT_BYTES) {
                Ok(data) if digest(&data) == *hash => verified.push((path, stamp)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Ok(_) => return Err(invalid("damaged retired record; cleanup stopped")),
                Err(e) => return Err(e),
            }
        }
        if !transaction(folder, revision, state, || {
            for (path, stamp) in verified {
                match FileStamp::read(&path) {
                    Ok(now) if now == stamp => fs::remove_file(path)?,
                    Ok(_) => return Err(invalid("retired record changed during cleanup")),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })? {
            return Ok(false);
        }
    }
    transaction(folder, revision, state, || {
        #[cfg(unix)]
        fs::File::open(folder)?.sync_all()?;
        let mut done = state.clone();
        done.pending.clear();
        done.write(folder)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_folder::{FolderSnapshot, RetentionPolicy, configure_retention, keep_version, pack};
    use filmcraft_project::{Project, ProjectView};

    #[test]
    fn a_stale_plan_cannot_retire_a_new_pin_or_unlink_a_record_reused_by_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        let source = dir.path().join("source.fcproj");
        let mut project = Project::new("Feature");
        let bin = project.add_bin("Cuts", None);
        let cut = project.new_sequence("Reel", Default::default(), 1, 1, Some(bin));
        fs::write(&source, crate::encode(&project, false)).unwrap();
        pack(&source, &folder, crate::atomic_write_new).unwrap();
        let (mut snapshot, loaded) = FolderSnapshot::open(&folder.join(ROOT)).unwrap();
        let base = snapshot.revision().to_owned();
        let policy = RetentionPolicy { enabled: false, max_bin_backups: 1, versions_per_bin: 1 };
        configure_retention(&folder, &base, policy).unwrap();
        let mut saved = loaded.project;
        let view = ProjectView::default();
        let mut save = |snapshot: &mut FolderSnapshot, frame| {
            let mut next = saved.clone();
            next.sequence_mut(cut).unwrap().start_timecode = frame;
            snapshot.save(&saved, &view, &next, &view).unwrap();
            saved = next;
        };
        for frame in 1..=4 {
            save(&mut snapshot, frame);
        }
        let head = snapshot.revision().to_owned();
        let state = Catalog::read(&folder).unwrap();
        let planned = plan(&folder, &state, policy, &head, None, || Ok(())).unwrap();
        assert!(planned.preview.retire.contains(&base));
        keep_version(&folder, &head, &base, true).unwrap();
        let mut published = state.clone();
        let publish = || publish_visibility(&folder, &head, &mut published, &planned);
        assert!(!transaction(&folder, &head, &state, publish).unwrap());
        keep_version(&folder, &head, &base, false).unwrap();
        assert!(transaction(&folder, &head, &state, || publish_visibility(&folder, &head, &mut published, &planned)).unwrap());
        // Mimic a restart between catalog publication and unlink. Reusing a
        // retired payload in a new edit makes the old list unsafe to apply.
        save(&mut snapshot, 1);
        let (_, latest) = revisions::read_base(&folder, snapshot.revision()).unwrap();
        let reused = &latest.items.iter().find(|(id, _)| *id == cut.0).unwrap().1;
        assert!(planned.garbage.contains(reused));
        assert!(!collect_garbage(&folder, &head, &published, &planned.garbage).unwrap());
        configure_retention(&folder, snapshot.revision(), RetentionPolicy { enabled: true, ..policy }).unwrap();
        snapshot.schedule_retention().unwrap();
        snapshot.wait_for_retention().unwrap();
        assert!(Catalog::read(&folder).unwrap().pending.is_empty());
        let (_, reopened) = FolderSnapshot::open(&folder.join(ROOT)).unwrap();
        assert_eq!(reopened.project.sequence(cut).unwrap().start_timecode, 1);
    }
}
