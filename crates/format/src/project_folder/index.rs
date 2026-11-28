//! Derived, revision-pinned name and relink impact index.
use super::{FolderDirectory, IndexResult as Result, Reader};
use crate::MAX_PROJECT_BYTES;
use filmcraft_media::reader::read_regular_file_bounded;
use filmcraft_project::{ItemId, ProjectItem};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
#[derive(Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub sequence: bool,
    // Older checksummed indexes are still readable. Refresh them once to classify every record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<RecordClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<ImpactRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_id: Option<u64>,
}
#[derive(Serialize, Deserialize)]
pub enum ImpactRecord {
    Other,
    Subclip(ItemId),
    Script(std::collections::BTreeSet<ItemId>),
    AutoSequence(std::collections::BTreeSet<ItemId>),
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordClass {
    Graphic,
    Media,
    NonMedia,
}
impl From<&ProjectItem> for Label {
    fn from(item: &ProjectItem) -> Self {
        use filmcraft_project::ItemKind;
        let class = match &item.kind {
            ItemKind::Graphic { .. } => RecordClass::Graphic,
            ItemKind::Media(_) => RecordClass::Media,
            _ => RecordClass::NonMedia,
        };
        let impact = match &item.kind {
            ItemKind::Subclip { parent, .. } => ImpactRecord::Subclip(*parent),
            ItemKind::Script(doc) => ImpactRecord::Script(doc.slates.iter().flat_map(|s| &s.takes).filter_map(|take| take.source).collect()),
            ItemKind::Sequence(seq) => {
                let sources: std::collections::BTreeSet<_> = seq.clips_with_auto_points().map(|clip| clip.item).collect();
                if sources.is_empty() { ImpactRecord::Other } else { ImpactRecord::AutoSequence(sources) }
            }
            _ => ImpactRecord::Other,
        };
        Self { max_id: Some(item.max_id()), impact: Some(impact), name: item.name.clone(), sequence: item.as_sequence().is_some(), class: Some(class) }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Index {
    pub manifest: String,
    pub labels: BTreeMap<ItemId, Label>,
    // The original preview index contained only the manifest and labels.
    #[serde(default)]
    pub keys: BTreeMap<ItemId, String>,
    #[serde(default)]
    pub checksum: Option<String>,
}

impl Index {
    pub fn digest(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&(&self.manifest, &self.keys, &self.labels))?)))
    }

    pub fn read(path: &Path) -> Result<Self> {
        let index: Self = serde_json::from_slice(&read_regular_file_bounded(path, MAX_PROJECT_BYTES)?)?;
        if let Some(checksum) = &index.checksum {
            if *checksum != index.digest()? || !index.keys.keys().eq(index.labels.keys()) {
                return Err("damaged name index; rebuild from the published project".into());
            }
        } else if !index.keys.is_empty() {
            return Err("incomplete name index".into());
        }
        Ok(index)
    }

    pub fn build(directory: &FolderDirectory, old: Option<Self>) -> Result<(Self, usize, u64)> {
        let mut old = old.filter(|index| index.checksum.is_some());
        let mut remaining = directory.remaining_item_bytes;
        let before = remaining;
        let mut labels = BTreeMap::new();
        let mut keys = BTreeMap::new();
        let mut decoded = 0;
        for (id, key) in directory.item_keys() {
            let reused = old
                .as_mut()
                .filter(|index| index.keys.get(&id).is_some_and(|k| k == key))
                .and_then(|index| index.labels.remove(&id))
                .filter(|label| label.class.is_some() && label.impact.is_some() && label.max_id.is_some());
            let label = match reused {
                Some(label) => label,
                None => {
                    decoded += 1;
                    Label::from(&directory.read_item(id, &mut remaining)?)
                }
            };
            labels.insert(id, label);
            keys.insert(id, key.to_owned());
        }
        let mut next = Index { manifest: directory.revision.clone(), labels, keys, checksum: None };
        next.checksum = Some(next.digest()?);
        Ok((next, decoded, before - remaining))
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        let data = serde_json::to_vec(self)?;
        if data.len() as u64 > MAX_PROJECT_BYTES {
            return Err("name index exceeds the size limit".into());
        }
        Ok(data)
    }
}

impl Index {
    pub fn impact_items(&self, source: ItemId) -> Result<BTreeSet<ItemId>> {
        let mut children: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (&id, label) in &self.labels {
            if let Some(ImpactRecord::Subclip(parent)) = &label.impact {
                children.entry(*parent).or_default().push(id);
            }
        }
        let mut users = BTreeSet::new();
        let mut pending = vec![source];
        while let Some(id) = pending.pop() {
            if users.insert(id)
                && let Some(subclips) = children.get(&id)
            {
                pending.extend(subclips);
            }
        }
        let mut selected = users.clone();
        for (&id, label) in &self.labels {
            match label.impact.as_ref().ok_or("incomplete impact index")? {
                ImpactRecord::Script(refs) if !refs.is_disjoint(&users) => {
                    selected.insert(id);
                }
                ImpactRecord::AutoSequence(refs) => {
                    // Frame-relative points resolve even when the clip uses unrelated media.
                    selected.insert(id);
                    for &clip in refs {
                        let mut id = clip;
                        while selected.insert(id) {
                            let label = self.labels.get(&id).ok_or("missing automatic point source")?;
                            if let Some(ImpactRecord::Subclip(parent)) = &label.impact {
                                id = *parent;
                            } else {
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(selected)
    }
}

// Resolve existing ancestors before creating anything, including when the data directory
// is overridden. A project must never acquire cache files just because it was previewed.
pub fn cache_directory(path: &Path, root: &Path) -> Result<PathBuf> {
    cache_path(path, root, true)
}

fn cache_path(path: &Path, root: &Path, create: bool) -> Result<PathBuf> {
    let project = fs::canonicalize(path.parent().ok_or("missing project folder")?)?;
    let key = Sha256::digest(project.as_os_str().as_encoded_bytes());
    let root = std::path::absolute(root)?;
    let existing = root.ancestors().find(|p| p.exists()).ok_or("missing cache volume")?;
    let root = fs::canonicalize(existing)?.join(root.strip_prefix(existing)?);
    let base = root.join("Preview indexes");
    let dir = base.join(format!("{key:x}"));
    if dir.starts_with(&project) || dir.components().any(|part| part == std::path::Component::ParentDir) {
        return Err("the preview cache must be outside the project folder".into());
    }
    if !create {
        for path in [&base, &dir] {
            if !fs::symlink_metadata(path)?.is_dir() {
                return Err("invalid index directory".into());
            }
        }
        return Ok(dir);
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.recursive(true).create(&root)?;
    // Never follow a redirected cache directory into an unrelated staging area.
    for path in [&base, &dir] {
        match builder.recursive(false).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(path)?.is_dir() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(dir)
}

/// Serialize publishers of the derived index, including a preview already refreshing it.
pub fn lock_cache(dir: &Path) -> Result<fs::File> {
    let lock = fs::File::options().create(true).truncate(false).read(true).write(true).open(dir.join("index.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err("another preview is refreshing this cache; reopen when it finishes".into()),
        Err(fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    Ok(lock)
}

/// Carry a verified cache across a local trim Save. Only the edited item key can change.
/// A missing or stale cache is left for ordinary discovery to rebuild.
pub fn refresh_saved_trim(path: &Path, root: &Path, before: &str, after: &str) -> Result<()> {
    let dir = cache_path(path, root, false)?;
    let _cache = lock_cache(&dir)?;
    let _project = super::lock_source_revision(path, after)?;
    let dest = dir.join("index.json");
    let old = Index::read(&dest)?;
    if old.manifest != before || old.checksum.is_none() || !old.labels.values().all(|l| l.max_id.is_some()) {
        return Ok(());
    }
    let directory = FolderDirectory::open(path)?;
    let (next, ..) = Index::build(&directory, Some(old))?;
    crate::atomic_write_no_follow(&dest, &next.bytes()?)?;
    Ok(())
}

// Apply may consume a published warm index but may never rebuild one to qualify an edit.
pub fn warm_index(path: &Path, root: &Path, reader: &Reader) -> Result<Index> {
    let path = std::path::absolute(path)?;
    let index =
        cache_path(&path, root, false).and_then(|dir| Index::read(&dir.join("index.json"))).map_err(|_| "full editor required: no usable warm index")?;
    if index.checksum.is_none()
        || index.manifest != reader.directory.revision
        || !reader.directory.item_keys().all(|(id, key)| index.keys.get(&id).is_some_and(|k| k == key))
        || !reader.inventory().eq(index.labels.keys().copied())
        || !index.labels.values().all(|label| label.class.is_some() && label.impact.is_some())
    {
        return Err("full editor required: the impact index is stale or incomplete".into());
    }
    Ok(index)
}
