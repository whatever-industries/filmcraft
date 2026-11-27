//! Developer-only project-folder conversion. See docs/project-files.md.
use filmcraft_format::{
    atomic_write_new,
    project_folder::{checkout, history, move_backups, pack, rescue, restore_copy, save, unpack},
};
use std::{
    io::{self, Write},
    path::Path,
};

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [command, source, destination] = args.as_slice() {
        let (src, dest) = (Path::new(source), Path::new(destination));
        if command == "pack" {
            return pack(src, dest, atomic_write_new);
        }
        if command == "checkout" {
            let revision = checkout(src, dest)?;
            return writeln!(io::stdout().lock(), "{revision}");
        }
        if command == "unpack" {
            return unpack(src, dest);
        }
    }
    if let [command, folder, expected, source] = args.as_slice()
        && command == "save"
        && let Some(expected) = expected.to_str()
    {
        let revision = save(Path::new(folder), expected, Path::new(source))?;
        return writeln!(io::stdout().lock(), "{revision}");
    }
    if let [command, folder] = args.as_slice()
        && command == "move-backups"
    {
        let mut out = io::stdout().lock();
        return move_backups(Path::new(folder), |rev, b| {
            writeln!(out, "{rev} {} {} {} {} {}", b.source, b.destination, b.sequence, b.saved_source, b.snapshot)
        });
    }
    if let [command, folder] = args.as_slice()
        && command == "history"
    {
        let mut out = io::stdout().lock();
        return history(Path::new(folder), |number, hash| writeln!(out, "{number} {hash}"));
    }
    if let [command, folder, base, revision, id] = args.as_slice()
        && command == "restore-copy"
        && let (Some(base), Some(revision), Some(id)) = (base.to_str(), revision.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        let (new_id, digest) = restore_copy(Path::new(folder), base, revision, id)?;
        return writeln!(io::stdout().lock(), "{new_id} {digest}");
    }
    if let [command, folder, revision, id, out] = args.as_slice()
        && command == "rescue"
        && let (Some(revision), Some(id)) = (revision.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        return rescue(Path::new(folder), revision, id, Path::new(out));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: project_folder pack SOURCE.fcproj NEW_FOLDER | unpack FOLDER NEW_FILE.fcproj | checkout FOLDER NEW_FILE.fcproj | save FOLDER BASE_REVISION EDITED.fcproj | history FOLDER | move-backups FOLDER | restore-copy FOLDER BASE_REVISION HISTORICAL_REVISION SEQUENCE_ID | rescue FOLDER REVISION SEQUENCE_ID NEW_FILE.fcproj",
    ))
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("project_folder: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
