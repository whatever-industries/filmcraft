//! Incremental saves from a verified editor snapshot.
pub(super) mod checkpoint;
use super::{
    BinCopy, MANIFEST_LIMIT, Manifest, MoveBackup, ROOT, check_hash, check_manifest, digest, invalid, join, load_payload, overlay, paths, read_member,
    read_records, revisions,
};
use crate::{Loaded, MAX_PROJECT_BYTES, SCHEMA_VERSION};
use filmcraft_project::{Bin, BinEntry, ItemKind, MediaRef, Project, ProjectItem, ProjectView};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

/// The saved revision and verified record files for a locally opened project.
pub struct FolderSnapshot {
    retention: super::retention::background::Maintenance,
    total_bin_backups: u64,
    manifest: PathBuf,
    revision: String,
    files: BTreeMap<String, FileStamp>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct FileStamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(invalid(format!("expected a regular file at {}", path.display())));
        }
        Self::from_metadata(&metadata)
    }

    fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[derive(Debug)]
pub struct SaveResult {
    pub revision: String,
    /// Bytes passed to successful publication writes.
    pub bytes: u64,
    pub move_backups: Vec<MoveBackup>,
    pub maintenance_warning: Option<String>,
}

fn hashes(manifest: &Manifest) -> impl Iterator<Item = &String> {
    std::iter::once(&manifest.envelope).chain(manifest.bins.iter().chain(&manifest.items).map(|(_, hash)| hash))
}

fn compatible_document(document: Value) -> io::Result<Loaded> {
    let schema = document.get("schema_version").and_then(Value::as_u64).ok_or_else(|| invalid("missing project schema"))?;
    if schema == u64::from(SCHEMA_VERSION) {
        let envelope = serde_json::from_value(document)?;
        return crate::validate_loaded(crate::loaded(envelope, SCHEMA_VERSION)).map_err(invalid);
    }
    crate::decode(&serde_json::to_vec(&document)?).map_err(invalid)
}

/// Source context captured before a sequence leaves a bin.
pub struct Departure<'a> {
    pub sequence: filmcraft_project::ItemId,
    pub source: filmcraft_project::BinId,
    pub destination: filmcraft_project::BinId,
    pub saved_source: &'a str,
    pub cut: DepartureCut<'a>,
}

pub enum DepartureCut<'a> {
    Working(&'a Project),
    Archived(&'a str),
}

#[derive(Default)]
pub struct TransferSave<'a> {
    pub copies: Vec<BinCopy>,
    pub departures: Vec<Departure<'a>>,
    pub selected_bin: Option<u64>,
    pub active_move_bases: std::collections::BTreeSet<String>,
}

impl FolderSnapshot {
    pub fn open(path: &Path) -> io::Result<(Self, Loaded)> {
        if path.file_name().is_none_or(|name| name != ROOT) {
            return Err(invalid("expected project.fcmod"));
        }
        let absolute = std::path::absolute(path)?;
        let folder = absolute.parent().ok_or_else(|| invalid("missing project folder"))?;
        let bytes = read_member(&absolute, MANIFEST_LIMIT)?;
        let manifest = serde_json::from_slice(&bytes)?;
        check_manifest(&manifest)?;
        let mut files = BTreeMap::new();
        for hash in hashes(&manifest) {
            check_hash(hash)?;
            files.insert(hash.clone(), FileStamp::read(&folder.join(hash))?);
        }
        let loaded = compatible_document(join(read_records(folder, &manifest)?)?)?;
        if hashes(&manifest).any(|hash| FileStamp::read(&folder.join(hash)).ok() != files.get(hash).copied()) {
            return Err(invalid("project records changed during open"));
        }
        let total_bin_backups = manifest.revision.as_ref().map_or(0, |r| r.total_bin_backups);
        Ok((Self { manifest: absolute, revision: digest(&bytes), files, retention: Default::default(), total_bin_backups }, loaded))
    }

    /// Resume due or interrupted automatic cleanup after an editor open.
    pub fn schedule_retention(&mut self) -> io::Result<()> {
        let folder = self.manifest.parent().ok_or_else(|| invalid("missing project folder"))?;
        self.retention.schedule(folder, self.total_bin_backups)
    }

    pub fn retention_warning(&self) -> Option<String> {
        self.retention.warning()
    }

    /// Wait for previously scheduled cleanup (batch clients and validation).
    pub fn wait_for_retention(&self) -> io::Result<()> {
        self.retention.wait()
    }

    pub fn directory(&self) -> io::Result<&Path> {
        self.manifest.parent().ok_or_else(|| invalid("missing project folder"))
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    fn verified_writer(&self) -> io::Result<(fs::File, Vec<u8>, Manifest, RecordWriter<'_>)> {
        let folder = self.manifest.parent().ok_or_else(|| invalid("missing project folder"))?;
        let guard = revisions::writer(folder)?;
        let (prior_bytes, prior) = revisions::read_base(folder, &self.revision)?;
        let mut delta = RecordWriter { folder, files: self.files.clone(), bytes: 0 };
        for hash in hashes(&prior) {
            delta.verify(hash)?;
        }

        Ok((guard, prior_bytes, prior, delta))
    }

    /// Keep a verified revision locked while committing a local recovery decision.
    pub fn while_current<T>(&self, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let (_guard, _, _, _) = self.verified_writer()?;
        action()
    }

    /// Compare the live model to the last saved model. Only changed records are
    /// encoded. The revision still commits the changes as one transaction.
    pub fn save(&mut self, before: &Project, before_view: &ProjectView, after: &Project, after_view: &ProjectView) -> io::Result<SaveResult> {
        self.save_transfers(before, before_view, after, after_view, TransferSave::default())
    }

    pub fn save_transfers(
        &mut self,
        before: &Project,
        before_view: &ProjectView,
        after: &Project,
        after_view: &ProjectView,
        moves: TransferSave<'_>,
    ) -> io::Result<SaveResult> {
        let (_guard, prior_bytes, prior, mut delta) = self.verified_writer()?;
        let folder = delta.folder;
        let mut before_bins = BinRecords::new(before)?;
        let mut after_bins = BinRecords::new(after)?;
        if after_bins.0.len().saturating_add(after.items.len()) > super::RECORD_LIMIT {
            return Err(invalid("too many project records"));
        }
        // Only an explicit saved arrival advances a move. A later rename of an
        // older bin can update its bin revision without changing this evidence.
        let mut arrivals = BTreeMap::new();
        for (bin, record) in &after_bins.0 {
            if before_bins.0.get(bin).is_some_and(|old| old["children"] == record["children"]) {
                continue;
            }
            let members =
                |r: &Value| -> std::collections::BTreeSet<u64> { r["children"].as_array().into_iter().flatten().filter_map(|e| e["Item"].as_u64()).collect() };
            let old = before_bins.0.get(bin).map(members).unwrap_or_default();
            for id in members(record).difference(&old) {
                if before.sequence(filmcraft_project::ItemId(*id)).is_some() {
                    arrivals.insert(*id, *bin);
                }
            }
        }
        let mut next = prior.clone();
        // Compare the physical source record in its original namespace. A later
        // edit that needs both IDs materializes the copy directly in the bin.
        next.copies.clear();
        for alias in &prior.copies {
            alias.replace(before_bins.0.get_mut(&alias.bin).ok_or_else(|| invalid("missing original bin"))?, true)?;
        }
        let mut aliases = prior.copies.clone();
        for alias in &moves.copies {
            if let Some(previous) = aliases.iter_mut().find(|a| a.bin == alias.bin && a.copy == alias.original) {
                previous.copy = alias.copy;
            } else {
                aliases.push(*alias);
            }
        }
        for alias in &aliases {
            let Some(bin) = after_bins.0.get_mut(&alias.bin) else { continue };
            let entries = bin["children"].as_array().ok_or_else(|| invalid("missing children"))?;
            if entries.iter().any(|e| e["Item"] == alias.copy) && !entries.iter().any(|e| e["Item"] == alias.original) {
                alias.replace(bin, true)?;
                next.copies.push(*alias);
            }
        }
        let backups = moves.departures.iter().map(|d| delta.backup(&prior, &self.revision, before, d)).collect::<io::Result<_>>()?;
        next.bins = delta.group(&prior.bins, &before_bins.0, &after_bins.0, PartialEq::eq, |_, _| Ok(()), "bin")?;
        let mut before_items: BTreeMap<_, _> = before.items.iter().map(|(id, item)| (id.0, item)).collect();
        let mut prior_items = prior.items.clone();
        next.identities.clear();
        for (id, hash) in &mut prior_items {
            let key = filmcraft_project::ItemId(*id);
            if let (Some(identity), Some(old), Some(item)) = (prior.identities.get(id), before.item(key), after.item(key)) {
                if same_item(old, item) {
                    next.identities.insert(*id, identity.clone());
                } else {
                    *hash = delta.edit_copy(*id, hash, identity, old, item)?;
                    before_items.insert(*id, item);
                }
            }
        }
        for alias in &moves.copies {
            let id = filmcraft_project::ItemId(alias.copy);
            let copy = after.item(id).ok_or_else(|| invalid("missing independent sequence"))?;
            let source = filmcraft_project::ItemId(alias.original);
            let mut original = before.item(source).cloned().ok_or_else(|| invalid("missing original sequence"))?;
            original.id = id;
            original.created = id.0;
            original.name.clone_from(&copy.name);
            if !same_item(&original, copy) {
                return Err(invalid("independent copy differs from the saved source cut"));
            }
            let (_, hash) = prior.items.iter().find(|(id, _)| *id == source.0).ok_or_else(|| invalid("missing original sequence record"))?;
            let stored = prior.identities.get(&source.0).map_or(source.0, |old| old.stored);
            next.identities.insert(id.0, super::CopyIdentity { stored, name: copy.name.clone() });
            prior_items.push((id.0, hash.clone()));
            before_items.insert(id.0, copy);
        }
        let after_items = after.items.iter().map(|(id, item)| (id.0, item)).collect();
        next.items = delta.group(
            &prior_items,
            &before_items,
            &after_items,
            |a, b| same_item(a, b),
            |id, item| {
                if item.id.0 != id {
                    return Err(invalid(format!("item {id}: mismatched id")));
                }
                if let Some(sequence) = item.as_sequence() {
                    sequence.check_bounds().map_err(invalid)?;
                }
                if let Some(script) = item.as_script() {
                    script.validate().map_err(invalid)?;
                }
                Ok(())
            },
            "item",
        )?;
        let old_meta = metadata(before, before_view)?;
        let new_meta = metadata(after, after_view)?;
        if old_meta != new_meta {
            next.envelope = delta.update(Some(&prior.envelope), Some(&old_meta), &new_meta, "project metadata")?;
        }
        let size = hashes(&next).try_fold(0u64, |size, hash| {
            let len = delta.files.get(hash).ok_or_else(|| invalid("missing prepared record"))?.len;
            Ok::<_, io::Error>(size.saturating_add(len))
        })?;
        if size > MAX_PROJECT_BYTES {
            return Err(invalid("project records exceed the folder size limit"));
        }
        let backed_up_bins = moves
            .selected_bin
            .map(|id| [id].into_iter().collect())
            .unwrap_or_else(|| revisions::changed_bins(&prior, &next, &after_bins.0, revisions::changed_attachments(&old_meta, &new_meta)));
        let revision = revisions::commit(
            folder,
            &prior_bytes,
            &prior,
            &mut next,
            revisions::Transfers { backed_up_bins, backups, arrivals, active_move_bases: moves.active_move_bases },
            |_| Ok(()),
            |path, data| {
                if path.ends_with(ROOT) {
                    crate::atomic_write(path, data)?;
                } else {
                    stage_record(path, data)?;
                }
                delta.bytes += data.len() as u64;
                Ok(())
            },
        )?;
        let files = hashes(&next).filter_map(|hash| delta.files.get_key_value(hash).map(|(k, v)| (k.clone(), *v))).collect();
        let bytes = delta.bytes;
        let changed = revision != self.revision;
        self.total_bin_backups = next.revision.as_ref().map_or(0, |r| r.total_bin_backups);
        let maintenance_warning = if changed { self.schedule_retention().err().map(|e| e.to_string()) } else { None };
        if let Some(message) = &maintenance_warning {
            log::warn!("project saved; recovery maintenance: {message}");
        }
        self.revision.clone_from(&revision);
        self.files = files;
        let move_backups = next.revision.map(|r| r.moves).unwrap_or_default();
        Ok(SaveResult { revision, bytes, move_backups, maintenance_warning })
    }
}

fn stage_record(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // The commit syncs the folder once after staging the payloads and the archive.
    #[cfg(unix)]
    {
        crate::atomic::stage_new_file(path, bytes)
    }
    #[cfg(not(unix))]
    {
        crate::atomic_write_new(path, bytes)
    }
}

struct RecordWriter<'a> {
    folder: &'a Path,
    files: BTreeMap<String, FileStamp>,
    bytes: u64,
}

impl RecordWriter<'_> {
    fn edit_copy(&mut self, id: u64, hash: &str, identity: &super::CopyIdentity, before: &ProjectItem, after: &ProjectItem) -> io::Result<String> {
        let mut budget = MAX_PROJECT_BYTES;
        let mut raw = load_payload(self.folder, hash, &mut budget)?;
        identity.apply(id, &mut raw)?;
        let next = overlay::record(&raw, &serde_json::to_value(before)?, &serde_json::to_value(after)?, "sequence copy")?;
        self.update(None, None, &next, "sequence copy")
    }

    fn backup(&mut self, prior: &Manifest, revision: &str, before: &Project, departure: &Departure<'_>) -> io::Result<MoveBackup> {
        let seq = departure.sequence;
        let entry = |snapshot: String| MoveBackup {
            sequence: seq.0,
            source: departure.source.0,
            destination: departure.destination.0,
            snapshot,
            saved_source: departure.saved_source.to_owned(),
        };
        if departure.saved_source != revision {
            super::recovery::verify_cut(self.folder, departure.saved_source, seq.0)?;
        }
        let project = match departure.cut {
            DepartureCut::Working(project) => project,
            DepartureCut::Archived(hash) => {
                if hash != departure.saved_source {
                    super::recovery::verify_cut(self.folder, hash, seq.0)?;
                }
                return Ok(entry(hash.to_owned()));
            }
        };
        // The verified parent manifest is already the exact recovery point for
        // a move with no unpublished source edits. Commit archives it durably.
        let mut seen = std::collections::BTreeSet::new();
        let mut required = vec![seq];
        let mut unchanged = project.luts == before.luts && project.settings == before.settings;
        while let Some(id) = required.pop() {
            if !seen.insert(id) {
                continue;
            }
            let item = project.item(id).ok_or_else(|| invalid("missing required move backup source"))?;
            unchanged &= before.item(id).is_some_and(|old| same_item(old, item))
                && before.transcripts.get(&id) == project.transcripts.get(&id)
                && before.narrations.get(&id) == project.narrations.get(&id)
                && before.source_graphics.get(&id) == project.source_graphics.get(&id);
            item.visit_source_items(|source| required.push(source));
        }
        if unchanged {
            return Ok(entry(revision.to_owned()));
        }
        let mut cut = Project::new(&project.name);
        cut.items.clear();
        cut.root = Bin { id: project.root.id, name: project.root.name.clone(), children: Vec::new() };
        cut.settings = project.settings.clone();
        cut.luts = project.luts.clone();
        cut.next_id = project.next_id;
        let prior_items: BTreeMap<_, _> = prior.items.iter().map(|(id, h)| (*id, h.as_str())).collect();
        let mut items = Vec::new();
        let mut identities = BTreeMap::new();
        for id in seen {
            let item = project.item(id).ok_or_else(|| invalid("missing required move backup source"))?;
            cut.items.insert(id, item.clone());
            cut.root.children.push(BinEntry::Item(id));
            if let Some(v) = project.transcripts.get(&id) {
                cut.transcripts.insert(id, v.clone());
            }
            if let Some(v) = project.narrations.get(&id) {
                cut.narrations.insert(id, v.clone());
            }
            if let Some(v) = project.source_graphics.get(&id) {
                cut.source_graphics.insert(id, v.clone());
            }
            let hash = prior_items.get(&id.0).copied();
            let old = before.item(id);
            let saved = match (hash, old) {
                (Some(hash), Some(old)) if same_item(old, item) => {
                    if let Some(identity) = prior.identities.get(&id.0) {
                        identities.insert(id.0, identity.clone());
                    }
                    hash.to_owned()
                }
                (Some(hash), Some(old)) if prior.identities.contains_key(&id.0) => self.edit_copy(id.0, hash, &prior.identities[&id.0], old, item)?,
                _ => self.update(hash, old, item, "move backup item")?,
            };
            items.push((id.0, saved));
        }
        let meta = metadata(&cut, &ProjectView::default())?;
        let envelope = self.update(None, None, &meta, "move backup metadata")?;
        let bin = BinRecords::new(&cut)?.0.remove(&cut.root.id.0).ok_or_else(|| invalid("missing move backup bin"))?;
        let bins = vec![(cut.root.id.0, self.update(None, None, &bin, "move backup bin")?)];
        let archive = Manifest { format: super::FORMAT.into(), layout: 1, revision: None, copies: Vec::new(), identities, envelope, bins, items };
        // Exercise the same decoder used by a future rescue before promising a backup.
        super::assemble(self.folder, &archive, &self.folder.join(ROOT))?;
        let snapshot = self.update(None, None, &archive, "move backup manifest")?;
        Ok(entry(snapshot))
    }

    fn verify(&mut self, hash: &str) -> io::Result<()> {
        check_hash(hash)?;
        let path = self.folder.join(hash);
        let stamp = FileStamp::read(&path)?;
        if self.files.get(hash) != Some(&stamp) {
            let bytes = read_member(&path, MAX_PROJECT_BYTES)?;
            if digest(&bytes) != hash {
                return Err(invalid(format!("damaged project record {hash}")));
            }
            if FileStamp::read(&path)? != stamp {
                return Err(invalid(format!("project record changed during save {hash}")));
            }
            self.files.insert(hash.to_owned(), stamp);
        }
        Ok(())
    }

    fn group<T: Serialize>(
        &mut self,
        prior: &[(u64, String)],
        before: &BTreeMap<u64, T>,
        after: &BTreeMap<u64, T>,
        same: impl Fn(&T, &T) -> bool,
        validate: impl Fn(u64, &T) -> io::Result<()>,
        label: &str,
    ) -> io::Result<Vec<(u64, String)>> {
        let digests: BTreeMap<_, _> = prior.iter().map(|(id, hash)| (*id, hash.as_str())).collect();
        after
            .iter()
            .map(|(&id, item)| {
                let hash = digests.get(&id).copied();
                let old = before.get(&id);
                if let (Some(hash), Some(old)) = (hash, old)
                    && same(old, item)
                {
                    return Ok((id, hash.to_owned()));
                }
                let path = format!("{label} {id}");
                validate(id, item)?;
                let hash = self.update(hash, old, item, &path)?;
                Ok((id, hash))
            })
            .collect()
    }

    fn update<T: Serialize>(&mut self, hash: Option<&str>, before: Option<&T>, after: &T, label: &str) -> io::Result<String> {
        let bytes = if let (Some(hash), Some(before)) = (hash, before) {
            // A previous editor save may already have stored the exact typed encoding.
            if digest(&serde_json::to_vec(before)?) == hash {
                serde_json::to_vec(after)?
            } else {
                let mut budget = MAX_PROJECT_BYTES;
                let raw = load_payload(self.folder, hash, &mut budget)?;
                let before = serde_json::to_value(before)?;
                if raw == before {
                    serde_json::to_vec(after)?
                } else {
                    let merged = overlay::record(&raw, &before, &serde_json::to_value(after)?, label)?;
                    serde_json::to_vec(&merged)?
                }
            }
        } else {
            serde_json::to_vec(after)?
        };
        let hash = digest(&bytes);
        let path = self.folder.join(&hash);
        match stage_record(&path, &bytes) {
            Ok(()) => {
                self.bytes += bytes.len() as u64;
                self.files.insert(hash.clone(), FileStamp::read(&path)?);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => self.verify(&hash)?,
            Err(error) => return Err(error),
        }
        Ok(hash)
    }
}

struct BinRecords(BTreeMap<u64, Value>);

impl BinRecords {
    fn new(project: &Project) -> io::Result<Self> {
        let mut bins = Self(BTreeMap::new());
        bins.add(project, &project.root, 0)?;
        Ok(bins)
    }

    fn add(&mut self, project: &Project, bin: &Bin, depth: usize) -> io::Result<()> {
        if depth > 64 {
            return Err(invalid("bin nesting exceeds the supported depth"));
        }
        let children = bin
            .children
            .iter()
            .map(|child| match child {
                BinEntry::Bin(child) => {
                    self.add(project, child, depth + 1)?;
                    Ok(json!({"Bin": child.id}))
                }
                BinEntry::Item(item) if project.items.contains_key(item) => Ok(json!({"Item": item})),
                BinEntry::Item(item) => Err(invalid(format!("bin references missing item {}", item.0))),
            })
            .collect::<io::Result<Vec<_>>>()?;
        if self.0.insert(bin.id.0, json!({"id":bin.id, "name":bin.name, "children": children})).is_some() {
            return Err(invalid(format!("duplicate bin id {}", bin.id.0)));
        }
        Ok(())
    }
}

fn metadata(project: &Project, view: &ProjectView) -> io::Result<Value> {
    // Exhaustively destructure Project so new persisted fields are accounted for here.
    let Project { name, custom_colors, custom_color_names, settings, root, items, next_id, luts, transcripts, search_bins, source_graphics, narrations, notes } =
        project;
    if *next_id > Project::MAX_NEXT_ID {
        return Err(invalid("project ID limit reached"));
    }
    let metadata = Project {
        name: name.clone(),
        custom_colors: custom_colors.clone(),
        custom_color_names: custom_color_names.clone(),
        settings: settings.clone(),
        root: Bin::default(),
        items: BTreeMap::new(),
        next_id: *next_id,
        luts: luts.clone(),
        transcripts: transcripts.clone(),
        search_bins: search_bins.clone(),
        source_graphics: source_graphics.clone(),
        narrations: narrations.clone(),
        notes: notes.clone(),
    };
    let mut value = serde_json::to_value(craft_fcproj_format::EnvelopeOut {
        format: crate::FORMAT_ID,
        schema_version: SCHEMA_VERSION,
        generator: crate::generator(),
        project: &metadata,
        view: Some(view),
    })?;
    value["project"]["root"] = json!(root.id);
    // Preserve the converter's lexical item-key order.
    let mut inventory: Vec<_> = items.keys().map(|id| (id.0.to_string(), id.0)).collect();
    inventory.sort_unstable();
    value["project"]["items"] = json!(inventory.into_iter().map(|(_, id)| id).collect::<Vec<_>>());
    Ok(value)
}

fn same_item(before: &ProjectItem, after: &ProjectItem) -> bool {
    match (&before.kind, &after.kind) {
        (ItemKind::Sequence(a), ItemKind::Sequence(b)) if Arc::ptr_eq(a, b) => {
            let ProjectItem { id, name, label, custom_label_color, kind: _, metadata, created, split } = before;
            *id == after.id
                && *name == after.name
                && *label == after.label
                && *custom_label_color == after.custom_label_color
                && *metadata == after.metadata
                && *created == after.created
                && *split == after.split
        }
        _ => before == after,
    }
}

/// Preserve file references when making a standard copy or detaching a recovered project.
pub fn detached_project(mut project: Project, from: &Path, to: Option<&Path>) -> io::Result<Project> {
    let from = std::path::absolute(from)?;
    let to = to.map(std::path::absolute).transpose()?;
    for item in project.items.values_mut() {
        if let Some(media) = item.as_media_mut() {
            for reference in std::iter::once(&mut media.media).chain(media.proxy.iter_mut()) {
                if let MediaRef::File { path } = reference {
                    if let Some(to) = &to {
                        if let Some(relocated) = paths::relocated(path, &from, to)? {
                            *path = relocated;
                        }
                    } else if let Ok(absolute) = craft_docpath::resolve(path, Some(&from)) {
                        *path = absolute.to_str().ok_or_else(|| invalid("media reference is not UTF-8"))?.to_owned();
                    }
                }
            }
        }
    }
    Ok(project)
}
