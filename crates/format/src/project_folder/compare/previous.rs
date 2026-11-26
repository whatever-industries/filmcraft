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

pub fn previous(folder: &Path, selected: &str, bin: u64) -> io::Result<Option<String>> {
    check_hash(selected)?;
    let mut verified: Option<VerifiedBin> = None;
    let mut started = false;
    let mut current = None;
    let mut previous = None;
    let mut answer = None;
    recovery::walk(folder, |hash, manifest| {
        started |= hash == selected;
        if !started {
            return Ok(false);
        }
        if !manifest.bins.iter().any(|(id, _)| *id == bin) {
            if hash == selected {
                return Err(invalid(format!("bin {bin} absent at {hash}")));
            }
            return Ok(true);
        }
        let state = match &verified {
            Some(bin) if bin.matches(manifest) => bin.state,
            _ => verified.insert(VerifiedBin::read(folder, manifest, bin).map_err(|e| invalid(format!("Cannot establish bin history at {hash}: {e}")))?).state,
        };
        let selected_state = current.get_or_insert(state);
        if previous.is_none() && *selected_state == state {
            return Ok(false);
        }
        if previous.as_ref().is_some_and(|p| p != &state) {
            return Ok(true);
        }
        previous = Some(state);
        answer = Some(hash.to_owned());
        Ok(false)
    })?;
    if !started {
        return Err(invalid("revision outside project history"));
    }
    Ok(answer)
}
