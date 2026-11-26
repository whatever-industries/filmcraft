//! Select the save which introduced the preceding distinct state of this bin.
use super::{Selection, Snapshot};
use crate::project_folder::{Manifest, bins::Entry, check_hash, invalid, recovery};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::Path,
};

fn signature(mut snapshot: Snapshot) -> io::Result<[u8; 32]> {
    if let Some((key, message)) = snapshot.errors.first() {
        return Err(invalid(format!("{key:?}: {message}")));
    }
    for (key, raw) in &mut snapshot.records {
        if let Entry::Item(id) = key
            && !snapshot.parents.contains_key(key)
            && !snapshot.filings.contains_key(id)
            && (raw.pointer("/kind/Media").is_some() || raw.pointer("/kind/Subclip").is_some())
            && let Some(obj) = raw.as_object_mut()
        {
            for field in ["metadata", "name", "label", "custom_label_color"] {
                obj.remove(field);
            }
        }
    }
    let records: Vec<_> = snapshot.records.iter().collect();
    let mut digest = Sha256::new();
    {
        let mut writer = io::BufWriter::new(&mut digest);
        serde_json::to_writer(&mut writer, &(records, &snapshot.sidecars))?;
        writer.flush()?;
    }
    Ok(digest.finalize().into())
}

// Reuse a verified bin only within this walk. The envelope includes the inventory,
// transcripts and shared resources; each record includes the IDs of its dependencies.
struct VerifiedBin {
    envelope: String,
    records: BTreeMap<Entry, String>,
    state: [u8; 32],
}

fn manifest_records(m: &Manifest) -> impl Iterator<Item = (Entry, &String)> {
    m.bins.iter().map(|(id, hash)| (Entry::Bin(*id), hash)).chain(m.items.iter().map(|(id, hash)| (Entry::Item(*id), hash)))
}

impl VerifiedBin {
    fn read(folder: &Path, manifest: &Manifest, bin: u64) -> io::Result<Self> {
        let snapshot = Snapshot::read(folder, manifest, Selection::Bin(bin))?;
        let records = manifest_records(manifest).filter(|(key, _)| snapshot.records.contains_key(key)).map(|(key, hash)| (key, hash.clone())).collect();
        Ok(Self { envelope: manifest.envelope.clone(), records, state: signature(snapshot)? })
    }

    fn matches(&self, manifest: &Manifest) -> bool {
        // Comparing every relevant entry also catches missing and repeated IDs.
        // A manifest with a different record order takes the regular verification path.
        self.envelope == manifest.envelope
            && manifest_records(manifest).filter(|(key, _)| self.records.contains_key(key)).eq(self.records.iter().map(|(key, hash)| (*key, hash)))
    }
}

/// The picker and its comparison endpoints come from one verified manifest chain.
/// A damaged bin can still leave a usable revision picker for an explicit comparison.
pub struct BinVersions<'a> {
    pub revisions: Vec<(u64, String)>,
    pub previous: io::Result<Option<VersionPair<'a>>>,
}

pub struct VersionPair<'a> {
    folder: &'a Path,
    selected: &'a str,
    from: String,
    before: Manifest,
    after: Manifest,
}

impl VersionPair<'_> {
    pub fn into_report(self, selection: Selection) -> io::Result<(String, super::Report)> {
        let a = super::read_snapshot(self.folder, &self.from, &self.before, selection)?;
        let b = super::read_snapshot(self.folder, self.selected, &self.after, selection)?;
        let report = super::compare_snapshots(&self.from, &a, self.selected, &b, selection)?;
        Ok((self.from, report))
    }
}

pub fn bin_versions<'a>(folder: &'a Path, selected: &'a str, bin: u64) -> io::Result<BinVersions<'a>> {
    lookup(folder, selected, bin, true)
}

pub fn previous(folder: &Path, selected: &str, bin: u64) -> io::Result<Option<String>> {
    check_hash(selected)?;
    Ok(lookup(folder, selected, bin, false)?.previous?.map(|pair| pair.from))
}

fn lookup<'a>(folder: &'a Path, selected: &'a str, bin: u64, list_all: bool) -> io::Result<BinVersions<'a>> {
    let mut failure = check_hash(selected).err();
    let mut verified: Option<VerifiedBin> = None;
    let mut after = None;
    let mut before = None;
    let mut current_state = None;
    let mut previous_state = None;
    let mut finished = false;
    let mut revisions = Vec::new();
    recovery::walk(folder, |hash, manifest| {
        if list_all {
            revisions.push((manifest.revision.as_ref().map_or(0, |r| r.number), hash.to_owned()));
        }
        if finished || failure.is_some() {
            return Ok(!list_all);
        }
        if hash != selected && after.is_none() {
            return Ok(false);
        }
        if !manifest.bins.iter().any(|(id, _)| *id == bin) {
            if hash == selected {
                failure = Some(invalid(format!("bin {bin} absent at {hash}")));
            }
            finished = true;
            return Ok(!list_all);
        }
        let state = match &verified {
            Some(cached) if cached.matches(&manifest) => cached.state,
            _ => match VerifiedBin::read(folder, &manifest, bin) {
                Ok(next) => verified.insert(next).state,
                Err(e) => {
                    failure = Some(invalid(format!("Cannot establish bin history at {hash}: {e}")));
                    return Ok(!list_all);
                }
            },
        };
        if hash == selected {
            current_state = Some(state);
            after = Some(manifest);
        } else if previous_state.is_some_and(|p| p != state) {
            finished = true;
        } else if current_state != Some(state) {
            previous_state = Some(state);
            before = Some((hash.to_owned(), manifest));
        }
        Ok(finished && !list_all)
    })?;
    let previous = match (failure, after, before) {
        (Some(e), _, _) => Err(e),
        (_, None, _) => Err(invalid("revision outside project history")),
        (_, Some(after), Some((from, before))) => Ok(Some(VersionPair { folder, selected, from, before, after })),
        _ => Ok(None),
    };
    Ok(BinVersions { revisions, previous })
}
