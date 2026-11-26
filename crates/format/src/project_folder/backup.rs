//! An independent copy of the history reachable from one published root.
use super::{MANIFEST_LIMIT, ROOT, assemble, check_hash, digest, invalid, outside_folder, pins, read_member, recovery};
use crate::{MAX_PROJECT_BYTES, atomic_write_new};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

/// Back up project metadata into a new directory and return the captured revision digest.
/// Immutable members are copied byte-for-byte. Media locations keep their stored spelling.
pub fn published_history(source: &Path, destination: &Path) -> io::Result<String> {
    outside_folder(source, destination)?;
    // Capture pins first. A concurrently published revision can only extend this ancestry.
    let pins = pins::read(source)?;
    let root = read_member(&source.join(ROOT), MANIFEST_LIMIT)?;
    fs::create_dir(destination)?;
    let mut copied = BTreeMap::new();
    let result = copy_history(source, destination, &root, &pins, &mut copied);
    if result.is_err() {
        // Remove only members created by this invocation. An interrupted process can
        // leave an unfinished directory, but cannot expose a partially usable root.
        for member in copied.keys() {
            let _ = fs::remove_file(destination.join(member));
        }
        let _ = fs::remove_dir(destination);
    }
    result
}

fn copy_history(source: &Path, dest: &Path, root: &[u8], pins: &[pins::Pin], copied: &mut BTreeMap<String, u64>) -> io::Result<String> {
    let head = digest(root);
    recovery::walk_from(source, root.to_vec(), |revision, manifest, bytes| {
        if revision != head && !copied.contains_key(revision) {
            atomic_write_new(&dest.join(revision), bytes)?;
            copied.insert(revision.to_owned(), bytes.len() as u64);
        }
        let mut remaining = MAX_PROJECT_BYTES;
        let mut record = |hash: &str| -> io::Result<()> {
            let size = if let Some(size) = copied.get(hash) {
                *size
            } else {
                check_hash(hash)?;
                let bytes = read_member(&source.join(hash), remaining)?;
                if digest(&bytes) != hash {
                    return Err(invalid(format!("SHA-256 mismatch for {hash}")));
                }
                atomic_write_new(&dest.join(hash), &bytes)?;
                let size = bytes.len() as u64;
                copied.insert(hash.to_owned(), size);
                size
            };
            remaining = remaining.checked_sub(size).ok_or_else(|| invalid("project exceeds record size budget"))?;
            Ok(())
        };
        record(&manifest.envelope).map_err(|e| io::Error::new(e.kind(), format!("revision {revision}, project metadata: {e}")))?;
        for (kind, entries) in [("bin", manifest.bins), ("item", manifest.items)] {
            for (id, hash) in entries {
                record(&hash).map_err(|e| io::Error::new(e.kind(), format!("revision {revision}, {kind} {id}: {e}")))?;
            }
        }
        Ok(false)
    })?;

    // Verify the backup from its own bytes, including the historical project structures.
    let target = dest.join(ROOT);
    let mut remaining_pins: BTreeSet<_> = pins.iter().map(|pin| pin.revision.as_str()).collect();
    recovery::walk_from(dest, root.to_vec(), |revision, manifest, _| {
        remaining_pins.remove(revision);
        assemble(dest, &manifest, &target).map_err(|e| io::Error::new(e.kind(), format!("backup revision {revision}: {e}")))?;
        Ok(false)
    })?;
    if !pins.is_empty() {
        pins::require_reachable(&remaining_pins)?;
        let bytes = serde_json::to_vec(pins)?;
        atomic_write_new(&dest.join(pins::FILE), &bytes)?;
        copied.insert(pins::FILE.to_owned(), bytes.len() as u64);
        if pins::read(dest)? != pins {
            return Err(invalid("backup milestone pins differ from the captured pins"));
        }
    }
    #[cfg(unix)]
    {
        fs::File::open(dest)?.sync_all()?;
        let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::File::open(parent)?.sync_all()?;
    }
    atomic_write_new(&target, root)?;
    Ok(head)
}
