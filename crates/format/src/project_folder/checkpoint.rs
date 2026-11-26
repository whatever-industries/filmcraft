//! Two-phase publication in a private recovery session directory. Only the descriptor
//! commits a generation. Payloads have no embedded location-dependent filename.
use super::{ROOT, check_hash, digest, invalid, read_member, relocate_document};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const DESCRIPTOR: &str = "folder-draft.json";
/// Data directory used by the modular journal worker within a FilmCraft profile.
pub fn data_directory(profile: &Path) -> PathBuf {
    profile.join("FolderDrafts")
}

const DESCRIPTOR_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub manifest: PathBuf,
    pub head: String,
    identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<super::BinScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
}

impl Source {
    pub fn capture(directory: &Path, head: &str) -> io::Result<Self> {
        let metadata = fs::metadata(directory)?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;
            Some(format!("{}:{}", metadata.dev(), metadata.ino()))
        };
        #[cfg(not(unix))]
        let identity = metadata.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t| t.as_nanos().to_string());
        Ok(Self { manifest: directory.join(ROOT), head: head.to_owned(), identity, scope: None, cursor: None })
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.identity.is_some() && self.identity == other.identity && self.manifest == other.manifest && self.head == other.head
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub source: Source,
    pub generation: u64,
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_name: Option<String>,
    payload: String,
}

impl Checkpoint {
    pub fn read(dir: &Path) -> io::Result<Self> {
        let descriptor: Self = serde_json::from_slice(&read_member(&dir.join(DESCRIPTOR), DESCRIPTOR_LIMIT)?)?;
        check_hash(&descriptor.payload)?;
        check_hash(&descriptor.source.head)?;
        if !descriptor.source.manifest.is_absolute() || descriptor.source.manifest.file_name().is_none_or(|p| p != ROOT) {
            return Err(invalid("invalid recovery source manifest"));
        }
        Ok(descriptor)
    }

    pub fn document_at(&self, dir: &Path, destination: &Path) -> io::Result<Vec<u8>> {
        let bytes = read_member(&dir.join(&self.payload), crate::MAX_PROJECT_BYTES)?;
        if digest(&bytes) != self.payload {
            return Err(invalid("damaged recovery payload"));
        }
        relocate_document(&bytes, &dir.join(DESCRIPTOR), destination)
    }

    pub fn publish(dir: &Path, source: Source, generation: u64, name: &str, bytes: &[u8]) -> io::Result<()> {
        Self::publish_with(dir, source, generation, name, bytes, crate::atomic_write_no_follow)
    }

    fn publish_with(
        dir: &Path,
        source: Source,
        generation: u64,
        name: &str,
        bytes: &[u8],
        commit: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let project = crate::decode(bytes).map_err(invalid)?.project;
        let bin_name =
            source.scope.as_ref().and_then(|s| project.root.find_bin(filmcraft_project::BinId(s.bin))).map(|bin| bin.name.chars().take(256).collect());
        let prior = match Self::read(dir) {
            Ok(c) => Some(c),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let next = Self { source, generation, project_name: name.into(), bin_name, payload: digest(bytes) };
        let descriptor = serde_json::to_vec(&next)?;
        if descriptor.len() as u64 > DESCRIPTOR_LIMIT {
            return Err(invalid("recovery descriptor is too large"));
        }
        crate::atomic_write_no_follow(&dir.join(&next.payload), bytes)?;
        if let Err(e) = commit(&dir.join(DESCRIPTOR), &descriptor) {
            if prior.as_ref().is_none_or(|old| old.payload != next.payload) {
                let _ = fs::remove_file(dir.join(&next.payload));
            }
            return Err(e);
        }
        if let Some(old) = prior
            && old.payload != next.payload
        {
            let _ = fs::remove_file(dir.join(old.payload));
        }
        Ok(())
    }

    pub fn clear(dir: &Path) -> io::Result<()> {
        let old = match Self::read(dir) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        fs::remove_file(dir.join(DESCRIPTOR))?;
        let _ = fs::remove_file(dir.join(old.payload));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn killed_between_payload_and_descriptor_recovers_previous_generation() {
        use std::process::{Command, Stdio};
        const CHILD: &str = "FILMCRAFT_TEST_INTERRUPTED_FOLDER_CHECKPOINT";
        if let Some(dir) = std::env::var_os(CHILD) {
            let dir = PathBuf::from(dir);
            let old = Checkpoint::read(&dir).unwrap();
            let bytes = crate::encode(&filmcraft_project::Project::new("Unpublished"), false);
            Checkpoint::publish_with(&dir, old.source, 2, "Unpublished", &bytes, |_, _| {
                fs::write(dir.join("payload-synced"), b"ready")?;
                let mut release = String::new();
                std::io::stdin().read_line(&mut release)?;
                Err(io::Error::other("parent terminated"))
            })
            .unwrap_err();
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let source = Source::capture(dir, &digest(b"manual save")).unwrap();
        let bytes = crate::encode(&filmcraft_project::Project::new("Acknowledged"), false);
        Checkpoint::publish(dir, source, 1, "Acknowledged", &bytes).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "project_folder::checkpoint::tests::killed_between_payload_and_descriptor_recovers_previous_generation"])
            .env(CHILD, dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !dir.join("payload-synced").exists() {
            if start.elapsed() > std::time::Duration::from_secs(60) || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("worker failed to reach descriptor publication");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        let recovered = Checkpoint::read(dir).unwrap();
        assert_eq!(recovered.generation, 1);
        let bytes = recovered.document_at(dir, &dir.join("out.fcproj")).unwrap();
        assert_eq!(crate::decode(&bytes).unwrap().project.name, "Acknowledged");
    }

    #[test]
    fn interrupted_publication_keeps_previous_document_until_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let source = Source::capture(dir, &digest(b"head")).unwrap();
        let first = crate::encode(&filmcraft_project::Project::new("First"), false);
        let next = crate::encode(&filmcraft_project::Project::new("Second"), false);
        let interrupt = |_: &Path, _: &[u8]| Err(io::Error::other("injected descriptor write failure"));
        assert!(Checkpoint::publish_with(dir, source.clone(), 1, "First", &first, interrupt).is_err());
        assert!(!dir.join(DESCRIPTOR).exists());
        Checkpoint::publish(dir, source.clone(), 1, "First", &first).unwrap();
        assert!(Checkpoint::publish_with(dir, source.clone(), 2, "Second", &next, interrupt).is_err());
        let read = || Checkpoint::read(dir).unwrap().document_at(dir, &dir.join("export.fcproj")).unwrap();
        assert_eq!(crate::decode(&read()).unwrap().project.name, "First");
        Checkpoint::publish(dir, source, 2, "Second", &next).unwrap();
        assert_eq!(crate::decode(&read()).unwrap().project.name, "Second");
        let path = dir.join(Checkpoint::read(dir).unwrap().payload);
        fs::write(&path, b"broken").unwrap();
        assert!(Checkpoint::read(dir).unwrap().document_at(dir, &dir.join("out.fcproj")).is_err());
        fs::remove_file(path).unwrap();
        assert!(Checkpoint::read(dir).unwrap().document_at(dir, &dir.join("out.fcproj")).is_err());
    }
}
