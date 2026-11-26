//! A shared-mode writer owns a bin and its direct sequences. Only publication and
//! ID reservation serialize across otherwise independent checkouts.
use super::{MANIFEST_LIMIT, Manifest, ProjectRecords, ROOT, digest, invalid, read_member, read_records, recovery, revisions};
use filmcraft_project::{ItemId, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io,
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BinScope {
    pub bin: u64,
    pub sequences: BTreeSet<u64>,
    // Direct source filings at checkout. Undo may restore these after a save.
    #[serde(skip)] // Reconstructed from the verified origin revision on recovery.
    filings: BTreeSet<u64>,
    pub origin: String,
    pub start: u64,
    pub end: u64,
}

pub(super) struct Claim {
    pub scope: BinScope,
    original_ids: BTreeSet<u64>,
    /// Copies present in this checkout's last verified publication. A crashed checkout
    /// may encounter an unrelated item allocated in its old range by a whole-project writer.
    published_copies: BTreeSet<u64>,
    _locks: Vec<File>,
}

fn head(folder: &Path) -> io::Result<(String, Manifest)> {
    let b = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
    Ok((digest(&b), serde_json::from_slice(&b)?))
}

fn publication(folder: &Path) -> io::Result<File> {
    let lock = revisions::lock_file(&folder.join(".shared-publish.lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn high_water(folder: &Path) -> io::Result<u64> {
    match read_member(&folder.join(".shared-next-id"), 64) {
        Ok(b) => Ok(serde_json::from_slice(&b)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

pub(super) fn copy_records(records: &ProjectRecords) -> ProjectRecords {
    ProjectRecords { envelope: records.envelope.clone(), items: records.items.clone(), bins: records.bins.clone() }
}

fn project(records: &ProjectRecords) -> io::Result<Project> {
    let bytes = serde_json::to_vec(&copy_records(records).into_document().map_err(invalid)?)?;
    crate::decode(&bytes).map(|loaded| loaded.project).map_err(invalid)
}

/// Allocate at edit time under the same interprocess lock as publication. A later Save
/// cannot mint a newer version for an older value. Microseconds are a lower bound only.
pub(super) fn field_order(folder: &Path, observed: u64) -> io::Result<u64> {
    let _guard = publication(folder)?;
    let path = folder.join(".field-edit-clock");
    let last: u64 = match read_member(&path, 64) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(invalid)?.as_micros();
    let order = last.max(observed).checked_add(1).ok_or_else(|| invalid("field edit clock exhausted"))?.max(u64::try_from(now).map_err(invalid)?);
    crate::atomic_write_no_follow(&path, &serde_json::to_vec(&order)?)?;
    Ok(order)
}

const RESERVATION_SIZE: u64 = 65_536;

impl BinScope {
    fn owns_id(&self, id: u64) -> bool {
        (self.start..self.end).contains(&id)
    }

    fn owns_lut(&self, key: &str) -> bool {
        key.strip_prefix("lut").and_then(|v| v.parse().ok()).is_some_and(|id| self.owns_id(id))
    }

    pub(super) fn verify(&self, base: &Manifest, latest: &Manifest) -> io::Result<()> {
        for bin in [true, false] {
            fn index(m: &Manifest, bin: bool) -> super::RecordIndex<'_> {
                super::record_index(if bin { &m.bins } else { &m.items })
            }
            let before = index(base, bin);
            let after = index(latest, bin);
            let check = |id: u64| {
                let hash = before.get(&id).copied().flatten();
                let stamp = |m: &Manifest| m.revision.as_ref().and_then(|r| r.stamp(bin, id));
                if hash.is_none() || hash != after.get(&id).copied().flatten() || stamp(base) != stamp(latest) {
                    return Err(invalid(format!("{} {id} changed since checkout", if bin { "bin" } else { "sequence" })));
                }
                Ok(())
            };
            if bin {
                check(self.bin)?;
            } else {
                self.sequences.iter().copied().try_for_each(check)?;
            }
        }
        Ok(())
    }

    /// Compare the actual mutation before the engine installs it, including linked edits.
    pub fn validate_edit(&self, before: &Project, after: &Project) -> io::Result<()> {
        if after.next_id < self.start || after.next_id > self.end {
            return Err(invalid("ID reservation exhausted; save, release, and check out the bin again"));
        }
        if self.project_onto(before, after)? != *after {
            return Err(invalid(format!("edit changes records outside bin {}", self.bin)));
        }
        Ok(())
    }

    /// Project only owned records onto another view, retaining children owned by other bins.
    pub fn project_onto(&self, before: &Project, after: &Project) -> io::Result<Project> {
        let mut expected = before.clone();
        expected.next_id = after.next_id;
        let owner = filmcraft_project::BinId(self.bin);
        // Items created by this checkout live in its exclusive reservation.
        expected.items.retain(|id, _| !self.owns_id(id.0) || after.items.contains_key(id));
        for (id, src) in &after.items {
            if self.owns_id(id.0) {
                if let Some(dest) = expected.items.get_mut(id) {
                    if src.is_source_clip() {
                        dest.merge_editorial_fields(src, Some(owner));
                    } else {
                        dest.clone_from(src);
                    }
                } else {
                    expected.items.insert(*id, src.clone());
                }
            } else if let Some(dest) = expected.items.get_mut(id) {
                dest.merge_editorial_fields(src, Some(owner));
            }
        }
        expected.luts.retain(|lut| !self.owns_lut(&lut.id));
        expected.luts.extend(after.luts.iter().filter(|lut| self.owns_lut(&lut.id)).cloned());
        macro_rules! owned_sidecars {
            ($field:ident) => {{
                expected.$field.retain(|id, _| !self.owns_id(id.0));
                expected.$field.extend(after.$field.iter().filter(|(id, _)| self.owns_id(id.0)).map(|(id, value)| (*id, value.clone())));
            }};
        }
        owned_sidecars!(transcripts);
        owned_sidecars!(source_graphics);
        owned_sidecars!(narrations);
        for id in &self.sequences {
            let id = ItemId(*id);
            let Some(filmcraft_project::ItemKind::Sequence(seq)) = after.items.get(&id).map(|i| &i.kind) else {
                return Err(invalid("checked-out sequence was removed"));
            };
            let item = expected.items.get_mut(&id).ok_or_else(|| invalid("missing checked-out sequence"))?;
            // Only the timeline content of the owned project item is editable.
            if let filmcraft_project::ItemKind::Sequence(q) = &mut item.kind {
                *q = seq.clone();
            }
        }
        let id = filmcraft_project::BinId(self.bin);
        let old = expected.root.find_bin_mut(id).ok_or_else(|| invalid("missing checked-out bin"))?;
        let new = after.root.find_bin(id).ok_or_else(|| invalid("checked-out bin was removed"))?;
        old.name.clone_from(&new.name);
        old.filed_sources.clone_from(&new.filed_sources);
        let key = |entry: &filmcraft_project::BinEntry| match entry {
            filmcraft_project::BinEntry::Bin(b) => (true, b.id.0),
            filmcraft_project::BinEntry::Item(i) => (false, i.0),
        };
        let mut children: BTreeMap<_, _> = std::mem::take(&mut old.children).into_iter().map(|v| (key(&v), v)).collect();
        let mut seen = BTreeSet::new();
        for entry in &new.children {
            let k = key(entry);
            if !seen.insert(k) {
                return Err(invalid("duplicate bin entry"));
            }
            if let Some(child) = children.remove(&k) {
                old.children.push(child);
            } else if !k.0
                && ((self.owns_id(k.1) && after.items.contains_key(&ItemId(k.1)))
                    || (self.filings.contains(&k.1) && before.root.parent_of(ItemId(k.1)).is_none() && before.items.contains_key(&ItemId(k.1))))
            {
                old.children.push(filmcraft_project::BinEntry::Item(ItemId(k.1)));
            } else {
                return Err(invalid("entry is outside the checked-out bin"));
            }
        }
        if children.keys().any(|(bin, id)| *bin || (!self.owns_id(*id) && !self.filings.contains(id))) {
            return Err(invalid(format!("edit changes records outside bin {}", self.bin)));
        }
        Ok(expected)
    }

    fn apply(&self, latest: &mut ProjectRecords, draft: &ProjectRecords) -> io::Result<()> {
        let new = draft.bins.get(&self.bin).ok_or_else(|| invalid("draft has no checked-out bin"))?;
        let members = |v: &Value| -> io::Result<Vec<String>> {
            let mut keys = Vec::new();
            let mut seen = BTreeSet::new();
            for entry in v["children"].as_array().ok_or_else(|| invalid("expected bin entries"))? {
                if !seen.insert(entry.to_string()) {
                    return Err(invalid("duplicate bin entry"));
                }
                if entry["Item"].as_u64().is_some_and(|id| self.filings.contains(&id) || self.owns_id(id)) {
                    continue;
                }
                keys.push(entry.to_string());
            }
            keys.sort();
            Ok(keys)
        };
        let requested: BTreeSet<u64> = new["children"]
            .as_array()
            .ok_or_else(|| invalid("expected bin entries"))?
            .iter()
            .filter_map(|entry| entry["Item"].as_u64())
            .filter(|id| self.filings.contains(id))
            .collect();
        if !requested.is_empty()
            && (!requested.iter().all(|id| latest.items.contains_key(id))
                || latest.bins.iter().filter(|(id, _)| **id != self.bin).any(|(_, bin)| {
                    bin["children"].as_array().into_iter().flatten().filter_map(|entry| entry["Item"].as_u64()).any(|id| requested.contains(&id))
                }))
        {
            return Err(invalid("source filing is missing or belongs to another bin"));
        }
        let deleted: BTreeSet<_> = latest.items.keys().filter(|id| self.owns_id(**id) && !draft.items.contains_key(*id)).copied().collect();
        for id in &deleted {
            latest.items.remove(id);
        }
        for (&id, src) in &draft.items {
            if self.owns_id(id) {
                if src["id"].as_u64() != Some(id) {
                    return Err(invalid("invalid duplicated media item"));
                }
                if src.pointer("/kind/Media").is_some() || src.pointer("/kind/Subclip").is_some() {
                    latest.items.entry(id).or_insert_with(|| src.clone());
                } else {
                    latest.items.insert(id, src.clone());
                }
            }
            if let Some(dest) = latest.items.get_mut(&id) {
                merge_fields(dest, src, self.bin)?;
            }
        }
        for field in recovery::SIDECARS {
            let dst = &mut latest.envelope["project"];
            if let Some(map) = dst.get_mut(field).and_then(Value::as_object_mut) {
                map.retain(|key, _| key.parse::<u64>().ok().is_none_or(|id| !self.owns_id(id)));
            }
            for (key, val) in draft.envelope["project"][field].as_object().into_iter().flatten() {
                if key.parse::<u64>().ok().is_some_and(|id| self.owns_id(id) && draft.items.contains_key(&id)) {
                    dst[field][key] = val.clone();
                }
            }
        }
        let owned = |lut: &Value| lut["id"].as_str().is_some_and(|id| self.owns_lut(id));
        let incoming = draft.envelope["project"]["luts"].as_array().into_iter().flatten().filter(|lut| owned(lut));
        let mut removed = BTreeSet::new();
        if let Some(library) = latest.envelope["project"].get_mut("luts").and_then(Value::as_array_mut) {
            library.retain(|lut| {
                if owned(lut) {
                    removed.insert(lut["id"].as_str().unwrap_or_default().to_owned());
                    false
                } else {
                    true
                }
            });
        }
        for lut in incoming {
            removed.remove(lut["id"].as_str().unwrap_or_default());
            let library = latest.envelope["project"]
                .as_object_mut()
                .ok_or_else(|| invalid("expected project"))?
                .entry("luts")
                .or_insert_with(|| Value::Array(Vec::new()));
            library.as_array_mut().ok_or_else(|| invalid("invalid LUT library"))?.push(lut.clone());
        }
        let inventory = latest.envelope["project"]["items"].as_array_mut().ok_or_else(|| invalid("missing item inventory"))?;
        inventory.retain(|v| v.as_u64().is_none_or(|id| !deleted.contains(&id)));
        let existing: BTreeSet<_> = inventory.iter().filter_map(Value::as_u64).collect();
        inventory.extend(draft.items.keys().filter(|id| self.owns_id(**id) && !existing.contains(id)).copied().map(Value::from));
        let links: Vec<ItemId> = serde_json::from_value(new.get("filed_sources").cloned().unwrap_or_else(|| serde_json::json!([])))?;
        if links.iter().any(|id| !latest.items.contains_key(&id.0)) || links.iter().collect::<BTreeSet<_>>().len() != links.len() {
            return Err(invalid("missing or repeated source filing"));
        }
        let bin = latest.bins.get_mut(&self.bin).ok_or_else(|| invalid("missing bin"))?;
        if members(bin)? != members(new)? {
            return Err(invalid("bin membership changed"));
        }
        if links.is_empty() {
            bin.as_object_mut().ok_or_else(|| invalid("expected bin"))?.remove("filed_sources");
        } else {
            bin["filed_sources"] = serde_json::to_value(links)?;
        }
        bin["name"] = new["name"].clone();
        bin["children"] = new["children"].clone();
        for id in &self.sequences {
            let sequence = draft.items.get(id).and_then(|i| i.pointer("/kind/Sequence")).ok_or_else(|| invalid(format!("missing sequence {id}")))?;
            let dest =
                latest.items.get_mut(id).and_then(|i| i.pointer_mut("/kind/Sequence")).ok_or_else(|| invalid(format!("missing latest sequence {id}")))?;
            dest.clone_from(sequence);
        }
        super::history_luts::verify_remaining(latest, &removed)?;
        Ok(())
    }
}

/// Update only the winning fields in the raw record, retaining unknown extensions.
fn merge_fields(dest: &mut Value, src: &Value, bin: u64) -> io::Result<()> {
    let Some(fields) = src.get("field_edits").and_then(Value::as_object) else { return Ok(()) };
    for (key, value) in fields {
        let stamp: filmcraft_project::FieldEdit = serde_json::from_value(value.clone())?;
        if stamp.bin.0 != bin || dest["field_edits"][key]["order"].as_u64().unwrap_or(0) >= stamp.order {
            continue;
        }
        match key.as_str() {
            "Name" => dest["name"] = src["name"].clone(),
            "Label" => {
                dest["label"] = src["label"].clone();
                if let Some(color) = src.get("custom_label_color") {
                    dest["custom_label_color"] = color.clone();
                } else if let Some(obj) = dest.as_object_mut() {
                    obj.remove("custom_label_color");
                }
            }
            _ => {
                if let Some(v) = src["metadata"].get(key) {
                    dest["metadata"][key] = v.clone();
                } else if let Some(obj) = dest["metadata"].as_object_mut() {
                    obj.remove(key);
                }
            }
        }
        dest["field_edits"][key] = value.clone();
    }
    Ok(())
}

fn published_reservations(project: &Project, scope: &BinScope) -> BTreeSet<u64> {
    project
        .items
        .keys()
        .map(|id| id.0)
        .chain(project.luts.iter().filter_map(|lut| lut.id.strip_prefix("lut")?.parse().ok()))
        .filter(|id| scope.owns_id(*id))
        .collect()
}

impl Claim {
    pub fn acquire(folder: &Path, bin: u64, prior: Option<(&str, &BinScope)>) -> io::Result<(Self, String, ProjectRecords)> {
        let gate = revisions::lock_file(&folder.join(".writer.lock"))?;
        gate.try_lock_shared().map_err(|e| io::Error::new(io::Error::from(e).kind(), "whole project is busy"))?;
        let _publication = publication(folder)?;
        let (rev, m) = head(folder)?;
        let raw = read_records(folder, &m)?;
        if raw.envelope.pointer("/project/root").and_then(Value::as_u64) == Some(bin) {
            return Err(invalid("choose a non-root bin"));
        }
        let children = raw.bins.get(&bin).and_then(|v| v["children"].as_array()).ok_or_else(|| invalid(format!("missing bin {bin}")))?;
        let sequences: BTreeSet<_> =
            children.iter().filter_map(|v| v["Item"].as_u64()).filter(|id| raw.items.get(id).is_some_and(|v| v.pointer("/kind/Sequence").is_some())).collect();
        let mut locks = vec![gate];
        for (kind, id) in std::iter::once(("bin", bin)).chain(sequences.iter().map(|i| ("sequence", *i))) {
            let lock = revisions::lock_file(&folder.join(format!(".shared-{kind}-{id}.lock")))?;
            lock.try_lock().map_err(|e| io::Error::new(io::Error::from(e).kind(), format!("{kind} {id} is busy")))?;
            locks.push(lock);
        }
        let high = high_water(folder)?;
        let (mut scope, original) = if let Some((base, s)) = prior {
            s.verify(&recovery::at(folder, base)?, &m)?;
            if s.bin != bin
                || s.sequences != sequences.iter().copied().filter(|id| !s.owns_id(*id)).collect()
                || s.end.checked_sub(s.start) != Some(RESERVATION_SIZE)
                || s.end > high
                || s.end > Project::MAX_NEXT_ID
            {
                return Err(invalid("checkpoint reservation or ownership changed"));
            }
            let original = project(&read_records(folder, &recovery::at(folder, &s.origin)?)?)?;
            if s.start < original.next_id {
                return Err(invalid("checkpoint reservation overlaps original project IDs"));
            }
            (s.clone(), original)
        } else {
            let original = project(&raw)?;
            let start = high.max(original.next_id);
            let end = start.checked_add(RESERVATION_SIZE).filter(|end| *end <= Project::MAX_NEXT_ID).ok_or_else(|| invalid("project ID space exhausted"))?;
            crate::atomic_write_no_follow(&folder.join(".shared-next-id"), &serde_json::to_vec(&end)?)?;
            (BinScope { bin, sequences, filings: BTreeSet::new(), start, end, origin: rev.clone() }, original)
        };
        let original_bin = original.root.find_bin(filmcraft_project::BinId(bin)).ok_or_else(|| invalid("missing original bin"))?;
        scope.filings = original_bin
            .children
            .iter()
            .filter_map(|entry| match entry {
                filmcraft_project::BinEntry::Item(id) if original.sequence(*id).is_none() => Some(id.0),
                _ => None,
            })
            .collect();
        let mut original_ids = BTreeSet::new();
        for seq in &scope.sequences {
            let item = original.item(ItemId(*seq)).ok_or_else(|| invalid("checkout origin has no owned sequence"))?;
            item.visit_ids(|id| {
                original_ids.insert(id);
            });
        }
        let published_copies = if let Some((base, _)) = prior {
            published_reservations(&project(&read_records(folder, &recovery::at(folder, base)?)?)?, &scope)
        } else {
            BTreeSet::new()
        };
        Ok((Self { scope, original_ids, published_copies, _locks: locks }, rev, raw))
    }

    pub fn project_draft(&self, current: &ProjectRecords, draft: &ProjectRecords, cursor: u64) -> io::Result<ProjectRecords> {
        // Validate recovered and live ID ranges against every record outside the lease.
        if cursor > self.scope.end || cursor < self.scope.start {
            return Err(invalid("draft allocator is outside its reserved range"));
        }
        let mut peer = project(current)?;
        let published_next = peer.next_id;
        // Filed rows and sequence links may legitimately reference a published copy.
        peer.items.retain(|id, _| !self.scope.sequences.contains(&id.0) && !self.published_copies.contains(&id.0));
        let mut collision = false;
        peer.visit_ids(|id| collision |= self.scope.owns_id(id) && !self.published_copies.contains(&id));
        if collision {
            return Err(invalid("reserved IDs overlap another project record"));
        }
        let mut merged = copy_records(current);
        self.scope.apply(&mut merged, draft)?;
        if cursor > self.scope.start {
            merged.envelope["project"]["next_id"] = published_next.max(cursor).into();
        }
        let project = project(&merged)?;
        // Verify references while the full set of sources is available.
        for id in &self.scope.sequences {
            let q = project.sequence(ItemId(*id)).ok_or_else(|| invalid("missing owned sequence"))?;
            let mut referenced = q
                .all_tracks()
                .chain(&q.submix_tracks)
                .flat_map(|t| t.items.iter().map(|clip| clip.item))
                .chain(q.multicam.iter().flat_map(|m| &m.cameras).filter_map(|c| c.source))
                .chain(q.merged.iter().flat_map(|m| m.video.iter().chain(&m.audio)).copied());
            if referenced.any(|id| !project.items.contains_key(&id)) {
                return Err(invalid(format!("sequence {id} references a missing item")));
            }
        }
        for (&id, item) in &project.items {
            if self.scope.owns_id(id.0) {
                let invalid_id = !(self.scope.start..cursor).contains(&id.0)
                    || item.as_media().is_some_and(|m| m.markers.iter().any(|mark| !(self.scope.start..cursor).contains(&mark.id.0)));
                if invalid_id {
                    return Err(invalid("duplicate clip ID outside reservation"));
                }
            }
            let mut missing = false;
            item.visit_references(|source| missing |= self.scope.owns_id(source.0) && !project.items.contains_key(&source));
            if missing {
                return Err(invalid("another clip still uses the duplicate"));
            }
        }
        for bin in merged.bins.values() {
            if bin["filed_sources"].as_array().into_iter().flatten().filter_map(Value::as_u64).any(|id| !merged.items.contains_key(&id)) {
                return Err(invalid("another bin still files the duplicate"));
            }
        }
        if project.nest_cycle().is_some() {
            return Err(invalid("combined edits would contain a sequence nesting cycle"));
        }
        for (id, item) in project.items.iter().filter(|(id, _)| self.scope.owns_id(id.0)) {
            let mut escaped = false;
            item.visit_ids(|key| escaped |= !project.items.contains_key(&ItemId(key)) && !(self.scope.start..cursor).contains(&key));
            let mut missing = false;
            item.visit_references(|key| missing |= !project.items.contains_key(&key));
            if escaped || missing {
                return Err(invalid(format!("recovered item {} has an invalid ID or source", id.0)));
            }
        }
        // Only the checked-out sequences supply identifiers from the draft. IDs in the
        // other records came from the verified published revision.
        let mut escaped = false;
        for id in &self.scope.sequences {
            if let Some(item) = project.item(ItemId(*id)) {
                item.visit_ids(|id| {
                    escaped |= !self.original_ids.contains(&id) && !project.items.contains_key(&ItemId(id)) && !(self.scope.start..cursor).contains(&id);
                });
            }
        }
        if escaped {
            return Err(invalid("new ID outside checkout reservation"));
        }
        Ok(merged)
    }

    pub fn publish(&mut self, folder: &Path, base: &str, draft: &ProjectRecords, cursor: u64) -> io::Result<(String, Project)> {
        let (rev, published) = self.publish_with(folder, base, draft, cursor, revisions::publish)?;
        self.published_copies = published_reservations(&published, &self.scope);
        Ok((rev, published))
    }

    fn publish_with(
        &self,
        folder: &Path,
        base: &str,
        draft: &ProjectRecords,
        cursor: u64,
        write: impl FnMut(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<(String, Project)> {
        let _publication = publication(folder)?;
        if high_water(folder)? < self.scope.end {
            return Err(invalid("checkout ID reservation is missing"));
        }
        let (head, m) = head(folder)?;
        self.scope.verify(&recovery::at(folder, base)?, &m)?;
        let current = read_records(folder, &m)?;
        let merged = self.project_draft(&current, draft, cursor)?;
        let visible = project(&merged)?;
        Ok((revisions::save_locked(folder, &head, &merged, write)?, visible))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_publication_preserves_head_and_checkout_for_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("input.fcproj");
        std::fs::write(&input, include_bytes!("../../tests/fixtures/v1-edit.fcproj")).unwrap();
        let folder = tmp.path().join("cut");
        super::super::pack(&input, &folder, crate::atomic_write_new).unwrap();
        let (mut claim, base, mut draft) = Claim::acquire(&folder, 1, None).unwrap();
        // An unrelated media record can occupy an unpublished reservation after a crash.
        let mut collided = copy_records(&draft);
        let id = claim.scope.start;
        let mut media = collided.items[&5].clone();
        media["id"] = id.into();
        collided.items.insert(id, media);
        collided.envelope["project"]["items"].as_array_mut().unwrap().push(id.into());
        assert!(claim.project_draft(&collided, &draft, id).err().unwrap().to_string().contains("reserved IDs overlap"));
        draft.bins.get_mut(&1).unwrap()["name"] = "Dailies".into();
        let before = std::fs::read(folder.join(ROOT)).unwrap();
        let err = claim
            .publish_with(&folder, &base, &draft, claim.scope.start, |path, bytes| {
                if path == folder.join(ROOT) {
                    return Err(io::Error::other("interrupted before manifest rename"));
                }
                revisions::publish(path, bytes)
            })
            .unwrap_err();
        assert!(err.to_string().contains("interrupted before manifest rename"));
        assert_eq!(std::fs::read(folder.join(ROOT)).unwrap(), before);
        assert!(Claim::acquire(&folder, 1, None).is_err());
        let (next, _) = claim.publish(&folder, &base, &draft, claim.scope.start).unwrap();
        assert_ne!(next, base);
        let (current, manifest) = head(&folder).unwrap();
        assert_eq!(next, current);
        assert_eq!(read_records(&folder, &manifest).unwrap().bins[&1]["name"], "Dailies");
    }
}
