//! Verified, on-demand reads of a current-schema modular project.
use super::{FolderDirectory, IndexResult as Result};
use crate::SCHEMA_VERSION;
use filmcraft_project::{Bin, BinEntry, BinId, ItemId, Project, ProjectItem, ProjectView};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Deserialize)]
enum Child {
    Bin(BinId),
    Item(ItemId),
}
#[derive(Deserialize)]
struct Directory {
    name: String,
    children: Vec<Child>,
}

pub struct Reader {
    pub directory: FolderDirectory,
    root: BinId,
    pub view: ProjectView,
    // Retain all global settings, LUTs, and attached metadata for the RSS measurement.
    envelope: Value,
    bins: BTreeMap<BinId, Directory>,
    pub items: BTreeMap<ItemId, ProjectItem>,
    pub bytes: u64,
    remaining: u64,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        let mut directory = FolderDirectory::open(path)?;
        let env = directory.envelope.take();
        if env["schema_version"] != SCHEMA_VERSION || env["format"] != "filmcraft.project" {
            return Err("unsupported project envelope".into());
        }
        let view = serde_json::from_value(env["view"].clone())?;
        let root = serde_json::from_value(env["project"]["root"].clone())?;
        let mut bins = BTreeMap::new();
        for (id, raw) in std::mem::take(&mut directory.bins) {
            bins.insert(BinId(id), serde_json::from_value::<Directory>(raw)?);
        }
        let ids: BTreeSet<_> = directory.item_keys().map(|(id, _)| id).collect();
        let inventory: Vec<ItemId> = serde_json::from_value(env["project"]["items"].clone())?;
        if inventory.len() != ids.len() || inventory.into_iter().collect::<BTreeSet<_>>() != ids {
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
                    Child::Item(item) if !ids.contains(item) => return Err("unknown bin item".into()),
                    _ => {}
                }
            }
        }
        if seen.len() != bins.len() {
            return Err("unreachable bin".into());
        }
        Ok(Self { remaining: directory.remaining_item_bytes, bytes: directory.bytes_read, directory, root, view, envelope: env, bins, items: BTreeMap::new() })
    }

    pub fn inventory(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.directory.item_keys().map(|(id, _)| id)
    }

    /// Assemble the resident model for desktop playback using ordinary open-time validation.
    pub fn take_loaded(&mut self) -> Result<crate::Loaded> {
        let mut project = self.take_project()?;
        project.items = std::mem::take(&mut self.items);
        Ok(crate::validate_loaded(crate::Loaded { project, view: Some(self.view.clone()), generator: None, schema_version: SCHEMA_VERSION })?)
    }

    pub fn take_project(&mut self) -> Result<Project> {
        fn bin_tree(reader: &Reader, id: BinId, depth: usize) -> Result<Bin> {
            if depth > 64 {
                return Err("bin nesting exceeds the preview limit".into());
            }
            let dir = &reader.bins[&id];
            let children = dir
                .children
                .iter()
                .map(|child| match child {
                    Child::Item(id) => Ok(BinEntry::Item(*id)),
                    Child::Bin(id) => Ok(BinEntry::Bin(bin_tree(reader, *id, depth + 1)?)),
                })
                .collect::<Result<_>>()?;
            Ok(Bin { id, name: dir.name.clone(), children })
        }
        let mut fields = self.envelope["project"].take();
        fields["root"] = serde_json::to_value(Bin::default())?;
        fields["items"] = serde_json::json!({});
        let mut project: Project = serde_json::from_value(fields)?;
        project.root = bin_tree(self, self.root, 0)?;
        Ok(project)
    }

    // Publish newly decoded records only after the entire dependency traversal succeeds.
    // A damaged deferred timeline can then be retried without trusting an incomplete cut.
    pub fn missing(&mut self, mut pending: Vec<ItemId>, resident: &BTreeMap<ItemId, ProjectItem>) -> Result<BTreeMap<ItemId, ProjectItem>> {
        let mut added = BTreeMap::new();
        let mut remaining = self.remaining;
        while let Some(id) = pending.pop() {
            if self.items.contains_key(&id) || resident.contains_key(&id) || added.contains_key(&id) {
                continue;
            }
            let item = self.directory.read_item(id, &mut remaining)?;
            item.visit_source_items(|source| pending.push(source));
            added.insert(id, item);
        }
        self.bytes += self.remaining - remaining;
        self.remaining = remaining;
        Ok(added)
    }

    pub fn load(&mut self, seeds: Vec<ItemId>) -> Result<()> {
        let mut added = self.missing(seeds, &BTreeMap::new())?;
        self.items.append(&mut added);
        Ok(())
    }

    pub fn startup(&mut self, all_tabs: bool) -> Result<()> {
        self.activate(self.root)?;
        let mut seeds = if all_tabs { self.view.open_sequences.clone() } else { Vec::new() };
        seeds.extend(self.view.active_sequence);
        self.load(seeds)
    }

    pub fn activate(&mut self, bin: BinId) -> Result<()> {
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
