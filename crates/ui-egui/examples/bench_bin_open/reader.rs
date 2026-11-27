//! Read-only experiment for the benchmark's freshly packed, current-schema fixtures.
//! The normal editor never receives this partial inventory.
use super::Result;
use filmcraft_format::{MAX_PROJECT_BYTES, SCHEMA_VERSION, project_folder::ROOT};
use filmcraft_media::reader::read_regular_file_bounded;
use filmcraft_project::{BinId, ItemId, ProjectItem, ProjectView};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    layout: u32,
    envelope: String,
    bins: Vec<(BinId, String)>,
    items: Vec<(ItemId, String)>,
    #[serde(default)]
    copies: Vec<Value>,
    #[serde(default)]
    identities: BTreeMap<u64, Value>,
}

#[derive(Deserialize)]
enum Child {
    Bin(BinId),
    Item(ItemId),
}
#[derive(Deserialize)]
struct Directory {
    id: BinId,
    #[serde(rename = "name")]
    _name: String,
    children: Vec<Child>,
}

pub(super) struct Reader {
    dir: PathBuf,
    root: BinId,
    pub(super) view: ProjectView,
    // Retain all global settings, LUTs, and attached metadata for the RSS measurement.
    _envelope: Value,
    bins: BTreeMap<BinId, Directory>,
    hashes: BTreeMap<ItemId, String>,
    pub(super) items: BTreeMap<ItemId, ProjectItem>,
    pub(super) bytes: u64,
}

fn payload(dir: &Path, hash: &str, bytes: &mut u64) -> Result<Vec<u8>> {
    if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err("invalid record digest".into());
    }
    let path = dir.join(hash);
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err("expected a regular record".into());
    }
    let data = read_regular_file_bounded(&path, MAX_PROJECT_BYTES.saturating_sub(*bytes))?;
    if format!("{:x}", Sha256::digest(&data)) != hash {
        return Err("record digest mismatch".into());
    }
    *bytes += data.len() as u64;
    Ok(data)
}

impl Reader {
    pub(super) fn open(path: &Path) -> Result<Self> {
        if path.file_name().is_none_or(|s| s != ROOT) {
            return Err("expected project.fcmod".into());
        }
        let dir = path.parent().ok_or("missing folder")?.to_owned();
        let data = read_regular_file_bounded(path, 16 << 20)?;
        let m: Manifest = serde_json::from_slice(&data)?;
        if m.format != "filmcraft.project-folder" || m.layout != 1 || !m.copies.is_empty() || !m.identities.is_empty() {
            return Err("benchmark requires a freshly packed fixture".into());
        }
        let mut bytes = data.len() as u64;
        let env: Value = serde_json::from_slice(&payload(&dir, &m.envelope, &mut bytes)?)?;
        if env["schema_version"] != SCHEMA_VERSION || env["format"] != "filmcraft.project" {
            return Err("unsupported benchmark envelope".into());
        }
        let view = serde_json::from_value(env["view"].clone())?;
        let root = serde_json::from_value(env["project"]["root"].clone())?;
        let mut bins = BTreeMap::new();
        for (id, hash) in m.bins {
            let b: Directory = serde_json::from_slice(&payload(&dir, &hash, &mut bytes)?)?;
            if b.id != id || bins.insert(id, b).is_some() {
                return Err("invalid bin directory".into());
            }
        }
        let count = m.items.len();
        let hashes: BTreeMap<_, _> = m.items.into_iter().collect();
        let inventory: Vec<ItemId> = serde_json::from_value(env["project"]["items"].clone())?;
        if count != hashes.len() || inventory.len() != count || inventory.into_iter().collect::<BTreeSet<_>>() != hashes.keys().copied().collect() {
            return Err("invalid item inventory".into());
        }
        let mut pending = vec![root];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                return Err("repeated bin".into());
            }
            for entry in &bins.get(&id).ok_or("unknown bin")?.children {
                match entry {
                    Child::Bin(child) => pending.push(*child),
                    Child::Item(item) if !hashes.contains_key(item) => return Err("unknown bin item".into()),
                    _ => {}
                }
            }
        }
        if seen.len() != bins.len() {
            return Err("unreachable bin".into());
        }
        Ok(Self { dir, root, view, _envelope: env, bins, hashes, items: BTreeMap::new(), bytes })
    }

    fn load(&mut self, mut pending: Vec<ItemId>) -> Result<()> {
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let item = match self.items.entry(id) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let hash = self.hashes.get(&id).ok_or("missing source in benchmark fixture")?;
                    let item: ProjectItem = serde_json::from_slice(&payload(&self.dir, hash, &mut self.bytes)?)?;
                    if item.id != id {
                        return Err("mismatched item identity".into());
                    }
                    if let Some(cut) = item.as_sequence() {
                        cut.check_bounds()?;
                    }
                    entry.insert(item)
                }
            };
            item.visit_source_items(|source| pending.push(source));
        }
        Ok(())
    }

    pub(super) fn startup(&mut self, all_tabs: bool) -> Result<()> {
        self.activate(self.root)?;
        let mut seeds = if all_tabs { self.view.open_sequences.clone() } else { Vec::new() };
        seeds.push(self.view.active_sequence.ok_or("missing saved active timeline")?);
        self.load(seeds)
    }

    pub(super) fn activate(&mut self, bin: BinId) -> Result<()> {
        let children = &self.bins.get(&bin).ok_or("unknown bin")?.children;
        let ids = children
            .iter()
            .filter_map(|entry| match entry {
                Child::Item(id) => Some(*id),
                Child::Bin(_) => None,
            })
            .collect();
        self.load(ids)
    }
}
