//! Read the stored original and proxy paths from one revision and probe them at this folder's location.
use super::{RECORD_LIMIT, ROOT, invalid, record_index, recovery};
use crate::MAX_PROJECT_BYTES;
use filmcraft_media::reader::open_regular_file;
use filmcraft_project::{ItemKind, MediaRef};
use serde::Serialize;
use std::{collections::BTreeMap, io, path::Path};

#[derive(Serialize)]
pub struct Report {
    pub revision: String,
    pub counts: BTreeMap<Availability, usize>,
    pub sources: Vec<Source>,
    pub item_errors: BTreeMap<u64, String>,
}

#[derive(Serialize)]
pub struct Source {
    pub id: u64,
    pub name: String,
    pub marked_offline: bool,
    pub availability: Availability,
    pub original: Probe,
    pub proxy: Option<Probe>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    OriginalAvailable,
    ProxyOnly,
    NeedsRelink,
    NeedsInspection,
    Generated,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Probe {
    Generated,
    Available { path: String },
    Missing { path: String },
    Refused { path: String, reason: String },
    Inaccessible { path: String, reason: String },
}

fn probe(reference: MediaRef, document: &Path) -> Probe {
    let MediaRef::File { path } = reference else { return Probe::Generated };
    let resolved = match craft_docpath::resolve_absolute(&path, Some(document)) {
        Ok(resolved) => resolved,
        Err(e) => return Probe::Refused { path, reason: e.to_string() },
    };
    match open_regular_file(&resolved) {
        Ok(_) => Probe::Available { path },
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Probe::Missing { path },
        Err(e) => Probe::Inaccessible { path, reason: e.to_string() },
    }
}

fn availability(original: &Probe, proxy: Option<&Probe>) -> Availability {
    match (original, proxy) {
        (Probe::Generated, _) => Availability::Generated,
        (Probe::Available { .. }, _) => Availability::OriginalAvailable,
        (_, Some(Probe::Available { .. })) => Availability::ProxyOnly,
        (Probe::Missing { .. }, None | Some(Probe::Missing { .. } | Probe::Generated)) => Availability::NeedsRelink,
        _ => Availability::NeedsInspection,
    }
}

pub fn inspect(folder: &Path, revision: &str) -> io::Result<Report> {
    let manifest = recovery::at(folder, revision)?;
    let document = std::path::absolute(folder.join(ROOT))?;
    let mut budget = MAX_PROJECT_BYTES;
    let (envelope, _) = recovery::metadata(folder, &manifest, &mut budget)?;
    let inventory = recovery::inventory(envelope.pointer("/project/items"))?;
    if inventory.len() > RECORD_LIMIT {
        return Err(invalid("too many project items"));
    }
    drop(envelope);
    let index = record_index(&manifest.items);
    let mut report = Report { revision: revision.to_owned(), counts: BTreeMap::new(), sources: Vec::new(), item_errors: BTreeMap::new() };
    for &id in inventory.iter().chain(index.keys().filter(|id| !inventory.contains(id))) {
        match recovery::read_item(folder, &index, &inventory, id, &mut budget) {
            Ok((item, _)) => {
                let ItemKind::Media(media) = item.kind else { continue };
                let original = probe(media.media, &document);
                let proxy = media.proxy.map(|r| probe(r, &document));
                let availability = availability(&original, proxy.as_ref());
                *report.counts.entry(availability).or_default() += 1;
                report.sources.push(Source { id, name: item.name, marked_offline: media.offline, availability, original, proxy });
            }
            Err(e) => {
                report.item_errors.insert(id, e.to_string());
            }
        }
    }
    Ok(report)
}
