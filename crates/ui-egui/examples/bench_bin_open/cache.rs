//! Per-project cache used only by the developer preview. Save never acquires this lock.
use super::{FolderDirectory, Index, Preview, Reader, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

const STAGE_PREFIX: &str = ".preview-index-stage-";

pub(super) fn open(path: &Path) -> Result<Preview> {
    let root = filmcraft_engine::autosave::default_data_dir().ok_or("no FilmCraft user data directory")?;
    open_in(path, &root, || Ok(()))
}

// Resolve existing ancestors before creating anything, including when the data directory
// is overridden. A project must never acquire cache files just because it was previewed.
fn cache_directory(path: &Path, root: &Path) -> Result<PathBuf> {
    let project = fs::canonicalize(path.parent().ok_or("missing project folder")?)?;
    let key = Sha256::digest(project.as_os_str().as_encoded_bytes());
    let root = std::path::absolute(root)?;
    let existing = root.ancestors().find(|p| p.exists()).ok_or("missing cache volume")?;
    let root = fs::canonicalize(existing)?.join(root.strip_prefix(existing)?);
    let base = root.join("Preview indexes");
    let dir = base.join(format!("{key:x}"));
    if dir.starts_with(&project) || dir.components().any(|part| part == std::path::Component::ParentDir) {
        return Err("the preview cache must be outside the project folder".into());
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.recursive(true).create(&root)?;
    // Never follow a redirected cache directory into an unrelated staging area.
    for path in [&base, &dir] {
        match builder.recursive(false).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(path)?.is_dir() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(dir)
}

fn open_in(path: &Path, data_root: &Path, mut after_staging: impl FnMut() -> Result<()>) -> Result<Preview> {
    let path = std::path::absolute(path)?;
    let dir = cache_directory(&path, data_root)?;
    let lock = fs::File::options().create(true).truncate(false).read(true).write(true).open(dir.join("index.lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err("another preview is refreshing this cache; reopen when it finishes".into()),
        Err(fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_name().as_encoded_bytes().starts_with(STAGE_PREFIX.as_bytes()) && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    let dest = dir.join("index.json");
    let mut revision = FolderDirectory::published_revision(&path)?;
    for _ in 0..3 {
        let prepared = (|| -> Result<(Preview, Option<NamedTempFile>)> {
            let reader = Reader::open(&path)?;
            revision.clone_from(&reader.directory.revision);
            // The cache is disposable. An unreadable or incomplete index is rebuilt from verified project records.
            let old = Index::read(&dest).ok().filter(|v| v.checksum.is_some());
            let (index, stage) = match old {
                Some(index) if index.manifest == revision && reader.inventory().eq(index.labels.keys().copied()) => (index, None),
                seed => {
                    let (index, _, _) = Index::build(&reader.directory, seed)?;
                    let mut stage = tempfile::Builder::new().prefix(STAGE_PREFIX).tempfile_in(&dir)?;
                    stage.write_all(&index.bytes()?)?;
                    stage.as_file().sync_all()?;
                    after_staging()?;
                    (index, Some(stage))
                }
            };
            Ok((Preview::from_index(&path, reader, index)?, stage))
        })();
        let current = FolderDirectory::published_revision(&path)?;
        if current != revision {
            revision = current;
            continue;
        }
        let (preview, stage) = prepared?;
        if let Some(stage) = stage {
            stage.persist(&dest).map_err(|error| error.error)?;
            #[cfg(unix)]
            fs::File::open(&dir)?.sync_all()?;
            let published = FolderDirectory::published_revision(&path)?;
            if published != revision {
                revision = published;
                continue;
            }
        }
        return Ok(preview);
    }
    Err(io::Error::other("the project kept changing during refresh; reopen the preview").into())
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
