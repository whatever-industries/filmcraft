//! Copy a previewed bin between modular folders as a single in-memory destination edit.
use super::{
    Manifest, ProjectRecords, RECORD_LIMIT, ROOT,
    bins::{Entry, read_subset},
    digest, encode_at,
    history_paste::{CopyOrigin, allocate, copy_items},
    invalid, paths, recovery,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

pub struct Import<'a> {
    pub folder: &'a Path,
    pub revision: &'a str,
    pub head: &'a str,
    pub bin: u64,
}

pub struct ImportedBin {
    pub id: u64,
    pub document: Vec<u8>,
    pub next_id: u64,
}

impl Import<'_> {
    // Scope provenance to a folder and its history. Identical numeric IDs in a
    // different project, or in an unrelated replacement at this path, aren't identities.
    fn snapshot(&self, folder: &Path) -> io::Result<(Manifest, String)> {
        super::check_hash(self.revision)?;
        let mut first = true;
        let mut selected = None;
        let mut namespace = None;
        recovery::walk(folder, |hash, manifest| {
            if first && hash != self.head {
                return Err(invalid("the source has a newer or different save; preview the bin again"));
            }
            first = false;
            if manifest.revision.is_none() {
                let mut key = hash.as_bytes().to_vec();
                key.extend_from_slice(folder.as_os_str().as_encoded_bytes());
                namespace = Some(digest(&key));
            }
            if hash == self.revision {
                selected = Some(manifest);
            }
            Ok(false)
        })?;
        selected.zip(namespace).ok_or_else(|| invalid("selected revision is outside the source history"))
    }

    pub fn prepare(&self, destination: &Path, parent: u64, current: &[u8], next: u64) -> io::Result<ImportedBin> {
        self.prepare_with_scope(destination, parent, current, next, None)
    }

    pub fn prepare_in_checkout(&self, destination: &super::FolderSnapshot, parent: u64, current: &[u8]) -> io::Result<ImportedBin> {
        let scope = destination.scope().filter(|s| s.bin == parent).ok_or_else(|| invalid("check out the destination bin first"))?;
        self.prepare_with_scope(destination.directory(), parent, current, destination.cursor(), Some(scope))
    }

    fn prepare_with_scope(&self, destination: &Path, parent: u64, current: &[u8], mut next: u64, scope: Option<&super::BinScope>) -> io::Result<ImportedBin> {
        let folder = self.folder.canonicalize()?;
        let destination = destination.canonicalize()?;
        if folder == destination {
            return Err(invalid("choose a different source project"));
        }
        let (manifest, namespace) = self.snapshot(&folder)?;
        let subset = read_subset(&folder, &manifest, self.bin)?;
        let mut source = subset.records;
        paths::rebase(&mut source, &folder.join(ROOT), &destination.join(ROOT))?;
        let mut live = ProjectRecords::split(current).map_err(invalid)?;
        if !live.bins.contains_key(&parent) {
            return Err(invalid(format!("missing destination bin {parent}")));
        }
        let selected = subset.filed.into_iter().collect::<Vec<_>>();
        let valid = match scope {
            Some(scope) => (scope.start..scope.end).contains(&next),
            None => live.envelope["project"]["next_id"].as_u64().is_some_and(|minimum| next >= minimum),
        };
        if !valid {
            return Err(invalid("invalid destination ID cursor"));
        }
        let copied = copy_items(&source, &selected, CopyOrigin::External(&namespace), &mut live, &mut next)?;
        if live.bins.len() + source.bins.len() + live.items.len() > RECORD_LIMIT {
            return Err(invalid("import exceeds the project record limit"));
        }
        let mut bins = BTreeMap::new();
        for &id in source.bins.keys() {
            bins.insert(id, allocate(&mut next)?);
        }
        if scope.is_some_and(|scope| next > scope.end) {
            return Err(invalid("import exceeds the checkout ID reservation"));
        }
        // A source may already have a home filing. Its row in the imported
        // bin then becomes an additional filing of that same destination ID.
        let mut homes: BTreeSet<_> =
            live.bins.values().flat_map(|v| v["children"].as_array().into_iter().flatten()).filter_map(|v| v["Item"].as_u64()).collect();
        for (old, mut raw) in source.bins {
            let mut children = Vec::new();
            let mut filings = Vec::new();
            let entries: Vec<Entry> = serde::Deserialize::deserialize(&raw["children"])?;
            for entry in entries {
                match entry {
                    Entry::Bin(id) => children.push(Entry::Bin(bins[&id])),
                    Entry::Item(id) => {
                        let mapped = copied.mapped[&id];
                        if homes.insert(mapped) {
                            children.push(Entry::Item(mapped));
                        } else {
                            filings.push(mapped);
                        }
                    }
                }
            }
            if let Some(extra) = raw.get("filed_sources") {
                let ids: Vec<u64> = serde::Deserialize::deserialize(extra)?;
                filings.extend(ids.into_iter().map(|id| copied.mapped[&id]));
            }
            let id = bins[&old];
            raw["id"] = id.into();
            raw["children"] = json!(children);
            if !filings.is_empty() {
                raw["filed_sources"] = json!(filings);
            }
            live.bins.insert(id, raw);
        }
        let imported = bins[&self.bin];
        live.bins
            .get_mut(&parent)
            .and_then(|v| v["children"].as_array_mut())
            .ok_or_else(|| invalid("invalid destination bin children"))?
            .push(json!({"Bin":imported}));
        live.envelope["project"]["next_id"] = next.into();
        Ok(ImportedBin { id: imported, document: encode_at(&destination, live, &destination.join(ROOT))?, next_id: next })
    }
}
