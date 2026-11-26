//! Select the save which introduced the preceding distinct state of this bin.
use super::{Selection, Snapshot};
use crate::project_folder::{bins::Entry, check_hash, digest, invalid, recovery};
use std::{io, path::Path};

fn signature(mut snapshot: Snapshot) -> io::Result<String> {
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
    let records: Vec<_> = snapshot.records.into_iter().collect();
    Ok(digest(&serde_json::to_vec(&(records, snapshot.sidecars))?))
}

pub fn previous(folder: &Path, selected: &str, bin: u64) -> io::Result<Option<String>> {
    check_hash(selected)?;
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
        let state = Snapshot::read(folder, manifest, Selection::Bin(bin))
            .and_then(signature)
            .map_err(|e| invalid(format!("Cannot establish bin history at {hash}: {e}")))?;
        let selected_state = current.get_or_insert_with(|| state.clone());
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
