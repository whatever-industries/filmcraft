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
    pub origin: String,
    pub start: u64,
    pub end: u64,
}

pub(super) struct Claim {
    pub scope: BinScope,
    original_ids: BTreeSet<u64>,
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

const RESERVATION_SIZE: u64 = 65_536;

impl BinScope {
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
        let key = |entry: &filmcraft_project::BinEntry| match entry {
            filmcraft_project::BinEntry::Bin(b) => (true, b.id.0),
            filmcraft_project::BinEntry::Item(i) => (false, i.0),
        };
        let mut children: BTreeMap<_, _> = std::mem::take(&mut old.children).into_iter().map(|v| (key(&v), v)).collect();
        for entry in &new.children {
            old.children.push(children.remove(&key(entry)).ok_or_else(|| invalid("checkout permits reordering existing entries only"))?);
        }
        if !children.is_empty() {
            return Err(invalid(format!("edit changes records outside bin {}", self.bin)));
        }
        Ok(expected)
    }

    fn apply(&self, latest: &mut ProjectRecords, draft: &ProjectRecords) -> io::Result<()> {
        let bin = latest.bins.get_mut(&self.bin).ok_or_else(|| invalid("missing bin"))?;
        let new = draft.bins.get(&self.bin).ok_or_else(|| invalid("draft has no checked-out bin"))?;
        let members = |v: &Value| -> io::Result<Vec<String>> {
            let mut keys: Vec<_> = v["children"].as_array().ok_or_else(|| invalid("expected bin entries"))?.iter().map(Value::to_string).collect();
            keys.sort();
            Ok(keys)
        };
        if members(bin)? != members(new)? {
            return Err(invalid("bin membership changed"));
        }
        bin["name"] = new["name"].clone();
        bin["children"] = new["children"].clone();
        for id in &self.sequences {
            let sequence = draft.items.get(id).and_then(|i| i.pointer("/kind/Sequence")).ok_or_else(|| invalid(format!("missing sequence {id}")))?;
            let dest =
                latest.items.get_mut(id).and_then(|i| i.pointer_mut("/kind/Sequence")).ok_or_else(|| invalid(format!("missing latest sequence {id}")))?;
            dest.clone_from(sequence);
        }
        Ok(())
    }
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
        let (scope, original) = if let Some((base, s)) = prior {
            s.verify(&recovery::at(folder, base)?, &m)?;
            if s.bin != bin || s.sequences != sequences || s.end.checked_sub(s.start) != Some(RESERVATION_SIZE) || s.end > high || s.end > Project::MAX_NEXT_ID
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
            (BinScope { bin, sequences, start, end, origin: rev.clone() }, original)
        };
        let mut original_ids = BTreeSet::new();
        for seq in &scope.sequences {
            let item = original.item(ItemId(*seq)).ok_or_else(|| invalid("checkout origin has no owned sequence"))?;
            item.visit_ids(|id| {
                original_ids.insert(id);
            });
        }
        Ok((Self { scope, original_ids, _locks: locks }, rev, raw))
    }

    pub fn project_draft(&self, current: &ProjectRecords, draft: &ProjectRecords, cursor: u64) -> io::Result<ProjectRecords> {
        // Validate recovered and live ID ranges against every record outside the lease.
        if cursor > self.scope.end || cursor < self.scope.start {
            return Err(invalid("draft allocator is outside its reserved range"));
        }
        let mut peer = project(current)?;
        let published_next = peer.next_id;
        peer.items.retain(|id, _| !self.scope.sequences.contains(&id.0));
        let mut collision = false;
        peer.visit_ids(|id| collision |= (self.scope.start..self.scope.end).contains(&id));
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
        if project.nest_cycle().is_some() {
            return Err(invalid("combined edits would contain a sequence nesting cycle"));
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

    pub fn publish(&self, folder: &Path, base: &str, draft: &ProjectRecords, cursor: u64) -> io::Result<String> {
        self.publish_with(folder, base, draft, cursor, revisions::publish)
    }

    fn publish_with(
        &self,
        folder: &Path,
        base: &str,
        draft: &ProjectRecords,
        cursor: u64,
        write: impl FnMut(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<String> {
        let _publication = publication(folder)?;
        if high_water(folder)? < self.scope.end {
            return Err(invalid("checkout ID reservation is missing"));
        }
        let (head, m) = head(folder)?;
        self.scope.verify(&recovery::at(folder, base)?, &m)?;
        let current = read_records(folder, &m)?;
        let merged = self.project_draft(&current, draft, cursor)?;
        revisions::save_locked(folder, &head, &merged, write)
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
        let (claim, base, mut draft) = Claim::acquire(&folder, 1, None).unwrap();
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
        let next = claim.publish(&folder, &base, &draft, claim.scope.start).unwrap();
        assert_ne!(next, base);
        let (current, manifest) = head(&folder).unwrap();
        assert_eq!(next, current);
        assert_eq!(read_records(&folder, &manifest).unwrap().bins[&1]["name"], "Dailies");
    }
}
