//! The modular project directory. Folder and bin IDs occupy distinct namespaces:
//! a mixed legacy bin can supply both the folder and its directly filed bin.
use crate::{Bin, BinEntry, BinId, Project};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectoryEntry {
    Folder(BinId),
    Bin(BinId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFolder {
    pub name: String,
    pub children: Vec<DirectoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDirectory {
    version: u32,
    pub root: BinId,
    pub folders: BTreeMap<BinId, ProjectFolder>,
}

impl ProjectDirectory {
    /// Project a standard bin tree without changing any saved record or allocating an editorial ID.
    pub fn from_legacy(root: &Bin) -> Self {
        let mut directory = Self { version: 1, root: root.id, folders: BTreeMap::new() };
        directory.import(root);
        directory
    }

    fn import(&mut self, bin: &Bin) -> DirectoryEntry {
        let has_children = bin.children.iter().any(|e| matches!(e, BinEntry::Bin(_)));
        if !has_children && bin.id != self.root {
            return DirectoryEntry::Bin(bin.id);
        }
        let mut children = Vec::new();
        let mut included_items = false;
        for entry in &bin.children {
            match entry {
                BinEntry::Bin(b) => children.push(self.import(b)),
                BinEntry::Item(_) if !included_items => {
                    children.push(DirectoryEntry::Bin(bin.id));
                    included_items = true;
                }
                BinEntry::Item(_) => {}
            }
        }
        self.folders.insert(bin.id, ProjectFolder { name: bin.name.clone(), children });
        DirectoryEntry::Folder(bin.id)
    }

    pub fn parent(&self, entry: DirectoryEntry) -> Option<BinId> {
        self.folders.iter().find_map(|(id, f)| f.children.contains(&entry).then_some(*id))
    }

    pub fn add(&mut self, parent: BinId, entry: DirectoryEntry) -> Result<(), String> {
        if self.parent(entry).is_some() {
            return Err("directory entry already has a parent".into());
        }
        self.folders.get_mut(&parent).ok_or("choose a project folder")?.children.push(entry);
        Ok(())
    }

    pub fn move_entry(&mut self, entry: DirectoryEntry, to: BinId) -> Result<(), String> {
        if !self.folders.contains_key(&to) {
            return Err("choose a project folder".into());
        }
        let parent = self.parent(entry).ok_or("unknown or unmovable directory entry")?;
        if parent == to {
            return Ok(());
        }
        if let DirectoryEntry::Folder(folder) = entry {
            let mut cursor = Some(to);
            while let Some(id) = cursor {
                if id == folder {
                    return Err("a folder cannot contain itself".into());
                }
                cursor = self.parent(DirectoryEntry::Folder(id));
            }
        }
        self.folders.get_mut(&parent).ok_or("missing source folder")?.children.retain(|e| *e != entry);
        self.add(to, entry)
    }

    /// Publish just the path of a newly saved bin, stopping at its nearest saved ancestor.
    pub fn copy_path(&mut self, from: &Self, entry: DirectoryEntry) -> Result<(), String> {
        let parent = from.parent(entry).ok_or("missing bin path")?;
        if !self.folders.contains_key(&parent) {
            self.copy_path(from, DirectoryEntry::Folder(parent))?;
            self.folders.insert(parent, ProjectFolder { name: from.folders[&parent].name.clone(), children: Vec::new() });
        }
        let siblings = &from.folders[&parent].children;
        let target = self.folders.get_mut(&parent).ok_or("missing destination folder")?;
        let following = siblings.iter().skip_while(|e| **e != entry).skip(1);
        let at = following.filter_map(|e| target.children.iter().position(|saved| saved == e)).next().unwrap_or(target.children.len());
        target.children.insert(at, entry);
        Ok(())
    }

    pub fn validate(&self, bin_ids: &BTreeSet<BinId>) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported modular directory version".into());
        }
        let mut folders = BTreeSet::new();
        let mut bins = BTreeSet::new();
        let mut pending = vec![(self.root, 0)];
        while let Some((id, depth)) = pending.pop() {
            if depth > 64 || !folders.insert(id) {
                return Err("invalid modular folder hierarchy".into());
            }
            for entry in &self.folders.get(&id).ok_or("missing modular folder")?.children {
                match *entry {
                    DirectoryEntry::Folder(f) => pending.push((f, depth + 1)),
                    DirectoryEntry::Bin(b) if !bin_ids.contains(&b) || !bins.insert(b) => return Err("missing or repeated editorial bin".into()),
                    DirectoryEntry::Bin(_) => {}
                }
            }
        }
        if folders.len() != self.folders.len() {
            return Err("unreachable modular folder".into());
        }
        Ok(())
    }
}

impl Project {
    /// Keep a mixed legacy parent's editorial bin visible after its last item leaves.
    /// Materialize only when re-inferring roles would lose an existing bin.
    pub fn preserve_editorial_bins(&mut self, before: &Project) {
        if self.modular_directory.is_some() || self.root == before.root {
            return;
        }
        let previous = before.directory();
        let mut current = ProjectDirectory::from_legacy(&self.root);
        let mut changed = false;
        for (id, folder) in &previous.folders {
            let bin = DirectoryEntry::Bin(*id);
            if let Some(at) = folder.children.iter().position(|entry| *entry == bin)
                && let Some(now) = current.folders.get_mut(id)
                && !now.children.contains(&bin)
            {
                now.children.insert(at.min(now.children.len()), bin);
                changed = true;
            }
        }
        if changed {
            self.modular_directory = Some(current);
        }
    }

    pub fn validate_directory(&self) -> Result<(), String> {
        let Some(dir) = &self.modular_directory else { return Ok(()) };
        let mut ids = BTreeSet::new();
        let mut stack = vec![&self.root];
        let exposed: BTreeSet<_> = dir
            .folders
            .values()
            .flat_map(|f| &f.children)
            .filter_map(|entry| match entry {
                DirectoryEntry::Bin(id) => Some(*id),
                _ => None,
            })
            .collect();
        while let Some(bin) = stack.pop() {
            if !ids.insert(bin.id) {
                return Err("duplicate backing bin".into());
            }
            for entry in &bin.children {
                match entry {
                    BinEntry::Item(_) if !exposed.contains(&bin.id) => return Err("directory hides filed items".into()),
                    BinEntry::Bin(b) => stack.push(b),
                    _ => {}
                }
            }
        }
        dir.validate(&ids)
    }

    pub fn create_project_folder(&mut self, parent: BinId, name: &str) -> Result<BinId, String> {
        if !self.directory().folders.contains_key(&parent) {
            return Err("choose a project folder".into());
        }
        let id = BinId(self.alloc_id());
        let directory = self.directory_mut();
        directory.add(parent, DirectoryEntry::Folder(id))?;
        directory.folders.insert(id, ProjectFolder { name: name.to_owned(), children: Vec::new() });
        self.validate_directory()?;
        Ok(id)
    }

    pub fn create_editorial_bin(&mut self, parent: BinId, name: &str) -> Result<BinId, String> {
        if !self.directory().folders.contains_key(&parent) {
            return Err("choose a project folder".into());
        }
        self.directory_mut();
        let id = self.add_bin(name, None);
        let directory = self.directory_mut();
        if parent != directory.root {
            directory.move_entry(DirectoryEntry::Bin(id), parent)?;
        }
        Ok(id)
    }
}
