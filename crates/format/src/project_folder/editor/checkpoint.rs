//! Immutable project records in a session-owned data file. The current
//! manifest owns its byte ranges until the next marker is durably published.
use super::{BinRecords, FileStamp, Manifest, compatible_document, hashes, metadata, same_item, stage_record};
use crate::project_folder::{
    FORMAT, MANIFEST_LIMIT, RECORD_LIMIT, check_hash, decode_payload, digest, invalid, is_digest, join, read_member, read_records_with,
};
use crate::{Loaded, MAX_PROJECT_BYTES};
use filmcraft_project::{Project, ProjectView};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

mod relink;

const RECORDS: &str = "checkpoint-records";
const DATA: &str = "records.bin";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Extent(u64, u64);

impl Extent {
    fn end(self) -> io::Result<u64> {
        self.0.checked_add(self.1).ok_or_else(|| invalid("invalid checkpoint extent"))
    }
    fn read(self, file: &mut File, limit: u64) -> io::Result<Vec<u8>> {
        if self.1 > limit || self.end()? > file.metadata()?.len() {
            return Err(invalid("checkpoint extent outside data file or project limit"));
        }
        file.seek(SeekFrom::Start(self.0))?;
        let mut bytes = Vec::new();
        file.take(self.1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != self.1 {
            return Err(invalid("truncated checkpoint record"));
        }
        Ok(bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    records: Manifest,
    // One entry per digest, in the same order as hashes(&records).
    locations: Vec<Extent>,
}

#[derive(Default)]
pub struct CheckpointStore {
    previous: Option<Arc<Project>>,
    item_hashes: BTreeMap<u64, String>,
    live: BTreeMap<String, Extent>,
    stamp: Option<FileStamp>,
    data: Option<(PathBuf, File)>,
    published: Option<(String, FileStamp)>,
}

pub struct CheckpointWrite {
    pub bytes: usize,
    pub serialize: Duration,
    pub write: Duration,
    /// The checkpoint was published. Surplus storage can be collected later.
    pub cleanup_error: Option<io::Error>,
}

fn directory(session: &Path, create: bool) -> io::Result<PathBuf> {
    let dir = session.join(RECORDS);
    if create {
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    if !fs::symlink_metadata(&dir)?.is_dir() {
        return Err(invalid("expected a recovery record directory"));
    }
    Ok(dir)
}

pub fn load_checkpoint(session: &Path, root: &str) -> io::Result<Loaded> {
    let dir = directory(session, false)?;
    check_hash(root)?;
    let bytes = read_member(&dir.join(root), MANIFEST_LIMIT)?;
    if digest(&bytes) != root {
        return Err(invalid("damaged recovery manifest"));
    }
    let index: Index = serde_json::from_slice(&bytes)?;
    if index.locations.len() != 1 + index.records.bins.len() + index.records.items.len() {
        return Err(invalid("missing checkpoint locations"));
    }
    let locations: BTreeMap<_, _> = hashes(&index.records).zip(index.locations).map(|(h, e)| (h.as_str(), e)).collect();
    let data_path = dir.join(DATA);
    FileStamp::read(&data_path)?;
    let mut file = File::open(data_path)?;
    let records = read_records_with(&index.records, |hash, budget| {
        let loc = locations.get(hash).ok_or_else(|| invalid("missing checkpoint record"))?;
        decode_payload(hash, loc.read(&mut file, *budget)?, budget)
    })?;
    compatible_document(join(records)?)
}

pub fn remove_checkpoint_records(session: &Path) -> io::Result<()> {
    match directory(session, false) {
        Ok(path) => fs::remove_dir_all(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// Reserve all ranges of the last published checkpoint, including any records
// superseded by this edit. Failed or interrupted writes may only occupy gaps.
struct FreeSpace {
    gaps: BTreeSet<(u64, u64)>, // (length, offset) for best-fit allocation
    end: u64,
}
impl FreeSpace {
    fn new(live: &BTreeMap<String, Extent>) -> io::Result<Self> {
        let mut free = Self { gaps: BTreeSet::new(), end: 0 };
        let mut ranges: Vec<_> = live.values().copied().collect();
        ranges.sort_unstable_by_key(|r| r.0);
        for extent in ranges {
            if extent.0 > free.end {
                free.gaps.insert((extent.0 - free.end, free.end));
            }
            free.end = extent.end()?;
        }
        Ok(free)
    }
    fn reserve(&mut self, len: u64) -> io::Result<Extent> {
        if let Some((size, start)) = self.gaps.range((len, 0)..).next().copied() {
            self.gaps.remove(&(size, start));
            let range = Extent(start, len);
            if size > len {
                self.gaps.insert((size - len, range.end()?));
            }
            Ok(range)
        } else {
            let range = Extent(self.end, len);
            self.end = range.end()?;
            Ok(range)
        }
    }
}

impl CheckpointStore {
    pub fn confirmed(&self, session: &Path, root: &str) -> bool {
        self.published.as_ref().is_some_and(|(hash, stamp)| hash == root && FileStamp::read(&session.join(RECORDS).join(root)).ok() == Some(*stamp))
            && self.stamp.is_some_and(|stamp| FileStamp::read(&session.join(RECORDS).join(DATA)).ok() == Some(stamp))
    }

    pub fn publish(
        &mut self,
        session: &Path,
        project: &Arc<Project>,
        publish_marker: impl FnOnce(&str, usize) -> io::Result<usize>,
    ) -> io::Result<CheckpointWrite> {
        let start = Instant::now();
        let bins = BinRecords::new(project)?;
        if bins.0.len().saturating_add(project.items.len()) > RECORD_LIMIT {
            return Err(invalid("too many checkpoint records"));
        }
        let mut pending = BTreeMap::new();
        let mut stage = |bytes: Vec<u8>| {
            let hash = digest(&bytes);
            if !self.live.contains_key(&hash) {
                pending.entry(hash.clone()).or_insert(bytes);
            }
            hash
        };
        let mut meta = metadata(project, &ProjectView::default())?;
        if let Some(obj) = meta.as_object_mut() {
            obj.remove("view");
        }
        let envelope = stage(serde_json::to_vec(&meta)?);
        let bins = bins.0.iter().map(|(id, r)| Ok((*id, stage(serde_json::to_vec(r)?)))).collect::<io::Result<_>>()?;
        let mut items = Vec::with_capacity(project.items.len());
        for (id, item) in &project.items {
            let hash = match (self.previous.as_ref().and_then(|p| p.item(*id)), self.item_hashes.get(&id.0)) {
                (Some(old), Some(hash)) if same_item(old, item) => hash.clone(),
                _ => stage(serde_json::to_vec(item)?),
            };
            items.push((id.0, hash));
        }
        let records = Manifest { format: FORMAT.into(), layout: 1, revision: None, copies: Vec::new(), identities: BTreeMap::new(), envelope, bins, items };
        self.publish_records(
            session,
            records,
            start.elapsed(),
            Some(project.clone()),
            |_, hash, needed| Ok(if needed { pending.remove(hash) } else { None }),
            publish_marker,
        )
    }

    fn publish_records(
        &mut self,
        session: &Path,
        records: Manifest,
        serialize: Duration,
        previous: Option<Arc<Project>>,
        mut record: impl FnMut(usize, &str, bool) -> io::Result<Option<Vec<u8>>>,
        publish_marker: impl FnOnce(&str, usize) -> io::Result<usize>,
    ) -> io::Result<CheckpointWrite> {
        let writing = Instant::now();
        let dir = directory(session, true)?;
        #[cfg(unix)]
        File::open(session)?.sync_all()?;
        let data_path = dir.join(DATA);
        if self.data.is_none() {
            // Never adopt an existing writable file. A failed first publication
            // retains this handle and can reuse its uncommitted space on retry.
            let file = OpenOptions::new().read(true).write(true).create_new(true).open(&data_path)?;
            self.data = Some((data_path.clone(), file));
        }
        let (_, file) = self.data.as_mut().filter(|(path, _)| path == &data_path).ok_or_else(|| invalid("recovery session changed"))?;
        let stamp = FileStamp::read(&dir.join(DATA))?;
        if FileStamp::from_metadata(&file.metadata()?)? != stamp {
            return Err(invalid("recovery data file was replaced"));
        }
        if !self.live.is_empty() && self.stamp != Some(stamp) {
            // An interrupted write may have extended the file. Verify every
            // previously live extent before using the old map again.
            for (hash, extent) in &self.live {
                if digest(&extent.read(file, MAX_PROJECT_BYTES)?) != *hash {
                    return Err(invalid("damaged checkpoint record"));
                }
            }
        }
        let mut free = FreeSpace::new(&self.live)?;
        let mut next = BTreeMap::<String, Extent>::new();
        let mut locations = Vec::new();
        let mut payload_bytes = 0usize;
        let mut total = 0u64;
        for (position, hash) in hashes(&records).enumerate() {
            let existing = next.get(hash).or_else(|| self.live.get(hash));
            let data = record(position, hash, existing.is_none())?;
            let extent = if let Some(existing) = existing {
                *existing
            } else {
                let data = data.ok_or_else(|| invalid("missing checkpoint record"))?;
                let extent = free.reserve(data.len() as u64)?;
                file.seek(SeekFrom::Start(extent.0))?;
                file.write_all(&data)?;
                payload_bytes += data.len();
                extent
            };
            total = total.saturating_add(extent.1);
            if total > MAX_PROJECT_BYTES {
                return Err(invalid("checkpoint exceeds project size limit"));
            }
            locations.push(extent);
            next.insert(hash.clone(), extent);
        }
        if payload_bytes > 0 {
            file.sync_all()?;
        }
        let index = Index { records, locations };
        let data = serde_json::to_vec(&index)?;
        if data.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid("recovery manifest exceeds size limit"));
        }
        let root = digest(&data);
        match stage_record(&dir.join(&root), &data) {
            Ok(()) => payload_bytes += data.len(),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && read_member(&dir.join(&root), MANIFEST_LIMIT)? == data => {}
            Err(e) => return Err(e),
        }
        #[cfg(unix)]
        File::open(&dir)?.sync_all()?;
        let index_stamp = FileStamp::read(&dir.join(&root))?;
        if FileStamp::from_metadata(&file.metadata()?)? != FileStamp::read(&data_path)? {
            return Err(invalid("recovery data file changed during checkpoint"));
        }
        let marker_bytes = publish_marker(&root, payload_bytes)?;
        self.published = Some((root.clone(), index_stamp));
        self.previous = previous;
        self.item_hashes = index.records.items.into_iter().collect();
        self.live = next;
        let cleanup = (|| -> io::Result<()> {
            #[cfg(unix)]
            File::open(session)?.sync_all()?;
            let end = self.live.values().try_fold(0u64, |end, e| e.end().map(|n| end.max(n)))?;
            if file.metadata()?.len() > end {
                file.set_len(end)?;
            }
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                if let Some(name) = entry.file_name().to_str()
                    && is_digest(name)
                    && name != root
                {
                    fs::remove_file(entry.path())?;
                }
            }
            Ok(())
        })();
        self.stamp = FileStamp::read(&dir.join(DATA)).ok();
        Ok(CheckpointWrite { bytes: payload_bytes + marker_bytes, serialize, write: writing.elapsed(), cleanup_error: cleanup.err() })
    }
}
