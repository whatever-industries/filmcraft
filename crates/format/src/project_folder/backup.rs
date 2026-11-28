//! Publish an independent copy of the retained metadata at one saved project head.
use super::{MANIFEST_LIMIT, Manifest, ROOT, assemble, digest, invalid, read_member, recovery, retention, verified_bytes};
use crate::{MAX_PROJECT_BYTES, atomic::stage_new_file, atomic_write_new};
use std::collections::{BTreeMap, BTreeSet};
use std::{fs, io, path::Path};

/// Copy immutable history without changing revision identities or stored media paths.
/// The destination must be new. Publish its root only after verifying its own records.
pub fn backup_history(source: &Path, destination: &Path) -> io::Result<String> {
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if parent.canonicalize()?.starts_with(source.canonicalize()?) {
        return Err(invalid("choose a backup destination outside the project folder"));
    }
    let root = read_member(&source.join(ROOT), MANIFEST_LIMIT)?;
    let head = digest(&root);
    let manifest: Manifest = serde_json::from_slice(&root)?;
    let catalog = retention::Catalog::read(source)?;
    if root != read_member(&source.join(ROOT), MANIFEST_LIMIT)? {
        return Err(io::Error::other("project changed while capturing backup; retry"));
    }

    fs::create_dir(destination)?;
    let mut copy = Copy { source, destination, sizes: BTreeMap::new() };
    let result = copy.publish(&root, &head, manifest, catalog, parent);
    if result.is_err() {
        for name in copy.sizes.keys() {
            let _ = fs::remove_file(destination.join(name));
        }
        let _ = fs::remove_dir(destination);
    }
    result.map(|()| head)
}

struct Copy<'a> {
    source: &'a Path,
    destination: &'a Path,
    sizes: BTreeMap<String, u64>,
}

impl Copy<'_> {
    fn publish(&mut self, root: &[u8], head: &str, manifest: Manifest, catalog: retention::Catalog, _parent: &Path) -> io::Result<()> {
        let mut chain = BTreeMap::new();
        let mut departures = BTreeSet::new();
        let mut required = manifest.revision.as_ref().map(|r| r.active_move_bases.clone()).unwrap_or_default();
        required.insert(head.to_owned());
        recovery::walk_from(self.source, head.to_owned(), manifest.clone(), |hash, m| {
            let number = m.revision.as_ref().map_or(0, |r| r.number);
            chain.insert(hash.to_owned(), number);
            if hash != head {
                self.manifest(hash)?;
            }
            if let Some(r) = &m.revision {
                for departure in &r.moves {
                    departures.insert(departure.snapshot.clone());
                    required.insert(departure.saved_source.clone());
                }
            }
            if catalog.available(number, hash) {
                self.payloads(m)?;
            }
            Ok(false)
        })?;
        let catalog = catalog.for_backup(&chain, &required)?;
        for hash in &departures {
            if let Some(number) = chain.get(hash) {
                if !catalog.available(*number, hash) {
                    return Err(invalid("retired sequence move backup"));
                }
            } else {
                self.manifest(hash)?;
                let m = retention::backup_manifest(self.destination, hash)?;
                self.payloads(&m)?;
                assemble(self.destination, &m, &self.destination.join(ROOT))?;
            }
        }
        // Reopen the copied manifests and payloads independently of the source.
        recovery::walk_from(self.destination, head.to_owned(), manifest, |hash, m| {
            if catalog.available(m.revision.as_ref().map_or(0, |r| r.number), hash) {
                assemble(self.destination, m, &self.destination.join(ROOT))?;
            }
            Ok(false)
        })?;
        let bytes = serde_json::to_vec(&catalog)?;
        stage_new_file(&self.destination.join(retention::INDEX), &bytes)?;
        self.sizes.insert(retention::INDEX.into(), bytes.len() as u64);
        if retention::Catalog::read(self.destination)? != catalog {
            return Err(invalid("backup retention catalog changed"));
        }
        #[cfg(unix)]
        {
            fs::File::open(self.destination)?.sync_all()?;
            fs::File::open(_parent)?.sync_all()?;
        }
        atomic_write_new(&self.destination.join(ROOT), root)
    }

    fn manifest(&mut self, hash: &str) -> io::Result<()> {
        let mut limit = MANIFEST_LIMIT;
        self.member(hash, &mut limit)
    }

    fn member(&mut self, hash: &str, remaining: &mut u64) -> io::Result<()> {
        if let Some(size) = self.sizes.get(hash) {
            *remaining = remaining.checked_sub(*size).ok_or_else(|| invalid("backup exceeds record size budget"))?;
        } else {
            let bytes = verified_bytes(self.source, hash, remaining)?;
            stage_new_file(&self.destination.join(hash), &bytes)?;
            self.sizes.insert(hash.to_owned(), bytes.len() as u64);
        }
        Ok(())
    }

    fn payloads(&mut self, m: &Manifest) -> io::Result<()> {
        let mut remaining = MAX_PROJECT_BYTES;
        for hash in retention::payloads(m) {
            self.member(hash, &mut remaining)?;
        }
        Ok(())
    }
}
