//! Named retention pins for published saves. Changing a pin never creates a project revision.
use super::{MANIFEST_LIMIT, ROOT, check_hash, invalid, read_member, recovery, revisions};
use crate::atomic_write;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io, path::Path};

pub(super) const FILE: &str = "revision-pins.json";
const MAX_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub revision: String,
    pub label: String,
}

fn check_label(label: &str) -> io::Result<()> {
    if label.trim().is_empty() || label.len() > 256 || label.chars().any(char::is_control) {
        return Err(invalid("pin label must contain visible text, at most 256 UTF-8 bytes, without control characters"));
    }
    Ok(())
}

pub(super) fn read(folder: &Path) -> io::Result<Vec<Pin>> {
    let bytes = match read_member(&folder.join(FILE), MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let pins: Vec<Pin> = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{FILE}: {e}")))?;
    let mut seen = BTreeSet::new();
    for pin in &pins {
        check_hash(&pin.revision)?;
        check_label(&pin.label)?;
        if !seen.insert(&pin.revision) {
            return Err(invalid(format!("duplicate pin for {}", pin.revision)));
        }
    }
    Ok(pins)
}

fn verify(folder: &Path, root: Vec<u8>, pins: &[Pin]) -> io::Result<()> {
    let mut remaining: BTreeSet<_> = pins.iter().map(|pin| pin.revision.as_str()).collect();
    recovery::walk_from(folder, root, |digest, _, _| {
        remaining.remove(digest);
        Ok(remaining.is_empty())
    })?;
    require_reachable(&remaining)
}

pub(super) fn require_reachable(remaining: &BTreeSet<&str>) -> io::Result<()> {
    if let Some(missing) = remaining.first() {
        return Err(invalid(format!("pinned revision {missing} is outside the project history")));
    }
    Ok(())
}

pub fn list(folder: &Path) -> io::Result<Vec<Pin>> {
    let pins = read(folder)?;
    verify(folder, read_member(&folder.join(ROOT), MANIFEST_LIMIT)?, &pins)?;
    Ok(pins)
}

pub fn pin(folder: &Path, revision: &str, label: &str) -> io::Result<()> {
    check_label(label)?;
    update(folder, revision, Change::Pin(label))
}

pub fn unpin(folder: &Path, revision: &str) -> io::Result<()> {
    update(folder, revision, Change::Unpin)
}

/// Relabel the pin only if its stored label still matches the caller's observation.
pub fn rename(folder: &Path, revision: &str, expected: &str, new_label: &str) -> io::Result<()> {
    check_label(new_label)?;
    update(folder, revision, Change::Rename { expected, new_label })
}

enum Change<'a> {
    Pin(&'a str),
    Unpin,
    Rename { expected: &'a str, new_label: &'a str },
}

fn update(folder: &Path, revision: &str, change: Change<'_>) -> io::Result<()> {
    check_hash(revision)?;
    if !folder.join(ROOT).symlink_metadata()?.is_file() {
        return Err(invalid("missing regular project manifest"));
    }
    // Take a separate lock: the desktop editor may own the document writer for its entire session.
    let lock = revisions::lock_file(&folder.join(".revision-pins.lock"))?;
    lock.lock()?;
    let mut pins = read(folder)?;
    let root = read_member(&folder.join(ROOT), MANIFEST_LIMIT)?;
    let index = pins.iter().position(|pin| pin.revision == revision);
    match (change, index) {
        (Change::Pin(label), Some(i)) if pins[i].label != label => return Err(invalid(format!("revision {revision} already has a different pin label"))),
        (Change::Pin(_), Some(_)) | (Change::Unpin, None) => return verify(folder, root, &pins),
        (Change::Pin(label), None) => pins.push(Pin { revision: revision.to_owned(), label: label.to_owned() }),
        (Change::Unpin, Some(i)) => {
            pins.remove(i);
        }
        (Change::Rename { .. }, None) => return Err(invalid(format!("revision {revision} is not pinned"))),
        (Change::Rename { expected, new_label }, Some(i)) => {
            if pins[i].label != expected {
                return Err(invalid(format!("pin label changed for revision {revision}; list pins and retry")));
            }
            if expected == new_label {
                return verify(folder, root, &pins);
            }
            new_label.clone_into(&mut pins[i].label);
        }
    }
    verify(folder, root, &pins)?;
    let bytes = serde_json::to_vec(&pins)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("revision pins exceed size limit"));
    }
    atomic_write(&folder.join(FILE), &bytes)
}
