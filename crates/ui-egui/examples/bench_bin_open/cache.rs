//! Per-project cache used only by the developer preview. Save never acquires this lock.
use super::{FolderDirectory, Index, Preview, Reader, Result};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    fs,
    io::{self, Write},
    path::Path,
};
use tempfile::NamedTempFile;

const STAGE_PREFIX: &str = ".preview-index-stage-";

pub(super) fn open(path: &Path) -> Result<Preview> {
    let root = filmcraft_engine::autosave::default_data_dir().ok_or("no FilmCraft user data directory")?;
    open_with(path, &root, || Ok(()), Preview::from_index).map(|(view, _)| view)
}

use filmcraft_format::project_folder::index::cache_directory;
pub(super) use filmcraft_format::project_folder::index::warm_index;

#[cfg(test)]
fn open_in(path: &Path, data_root: &Path, after_staging: impl FnMut() -> Result<()>) -> Result<Preview> {
    open_with(path, data_root, after_staging, Preview::from_index).map(|(view, _)| view)
}

#[derive(Default)]
pub(super) struct CacheCost {
    pub project_bytes: u64,
    pub records: usize,
    pub cache_written: u64,
}

pub(super) fn open_with<T>(
    path: &Path,
    data_root: &Path,
    mut after_staging: impl FnMut() -> Result<()>,
    mut make: impl FnMut(&Path, Reader, Index) -> Result<T>,
) -> Result<(T, CacheCost)> {
    let path = std::path::absolute(path)?;
    let dir = cache_directory(&path, data_root)?;
    let _lock = filmcraft_format::project_folder::index::lock_cache(&dir)?;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_name().as_encoded_bytes().starts_with(STAGE_PREFIX.as_bytes()) && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    let dest = dir.join("index.json");
    let mut cost = CacheCost::default();
    // The directory reader accounts for its own manifest, envelope and bin reads.
    let revision_check = |cost: &mut CacheCost| -> Result<String> {
        let (revision, bytes) = FolderDirectory::published_revision_size(&path)?;
        cost.project_bytes += bytes;
        Ok(revision)
    };
    let mut revision = revision_check(&mut cost)?;
    for _ in 0..3 {
        let prepared = (|| -> Result<(T, Option<NamedTempFile>)> {
            let reader = Reader::open(&path)?;
            cost.project_bytes += reader.bytes;
            revision.clone_from(&reader.directory.revision);
            // The cache is disposable. An unreadable or incomplete index is rebuilt from verified project records.
            let old = Index::read(&dest).ok().filter(|v| v.checksum.is_some());
            let (index, stage) = match old {
                Some(index)
                    if index.manifest == revision
                        && index.labels.values().all(|l| l.class.is_some() && l.impact.is_some() && l.max_id.is_some())
                        && reader.inventory().eq(index.labels.keys().copied()) =>
                {
                    (index, None)
                }
                seed => {
                    let (index, records, bytes) = Index::build(&reader.directory, seed)?;
                    cost.records += records;
                    cost.project_bytes += bytes;
                    let mut stage = tempfile::Builder::new().prefix(STAGE_PREFIX).tempfile_in(&dir)?;
                    let encoded = index.bytes()?;
                    stage.write_all(&encoded)?;
                    cost.cache_written += encoded.len() as u64;
                    stage.as_file().sync_all()?;
                    after_staging()?;
                    (index, Some(stage))
                }
            };
            Ok((make(&path, reader, index)?, stage))
        })();
        let current = revision_check(&mut cost)?;
        if current != revision {
            revision = current;
            continue;
        }
        let (preview, stage) = prepared?;
        if let Some(stage) = stage {
            stage.persist(&dest).map_err(|error| error.error)?;
            #[cfg(unix)]
            fs::File::open(&dir)?.sync_all()?;
            let published = revision_check(&mut cost)?;
            if published != revision {
                revision = published;
                continue;
            }
        }
        return Ok((preview, cost));
    }
    Err(io::Error::other("the project kept changing during refresh; reopen the preview").into())
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
