//! Publish immutable records before a saved manifest or recovery marker names them.
use std::{io, path::Path};

#[cfg(unix)]
use std::fs;

pub(super) fn stage_record(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // The commit syncs the folder once after staging the payloads and the archive.
    #[cfg(unix)]
    {
        match crate::atomic::staged_contents(path, bytes)?.persist_noclobber(path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::Unsupported => publish_writer_owned_record(error.file, path),
            Err(error) => Err(error.error),
        }
    }
    #[cfg(not(unix))]
    {
        crate::atomic_write_new(path, bytes)
    }
}

/// The caller owns these internal names through the project writer lock, a private
/// checkpoint session, or a newly reserved conversion folder. Only that writer can
/// publish them. User-selected output paths have no such reservation.
#[cfg(unix)]
fn publish_writer_owned_record(staged: tempfile::NamedTempFile, path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    staged.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn writer_owned_publication_preserves_occupied_names_on_filesystems_without_exclusive_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record");
        let publish = |bytes| publish_writer_owned_record(crate::atomic::staged_contents(&path, bytes).unwrap(), &path);
        publish(b"original").unwrap();
        assert_eq!(publish(b"other").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"original");

        fs::remove_file(&path).unwrap();
        let missing = dir.path().join("absent");
        symlink(&missing, &path).unwrap();
        assert_eq!(publish(b"other").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_link(&path).unwrap(), missing);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
