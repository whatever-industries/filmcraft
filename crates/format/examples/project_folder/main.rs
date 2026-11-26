//! Developer-only project-folder conversion. See docs/project-files.md.
use filmcraft_format::project_folder as converter;

use converter::{
    bins::{list_bins, preview_bin, rescue_bin},
    compare::{Selection, compare},
    pack,
    recovery::{history, rescue, restore_copy},
    revisions::{checkout, save},
    unpack,
};
use filmcraft_format::atomic_write_new;
use std::{io, path::Path};

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [command, source, destination] = args.as_slice() {
        let (src, dest) = (Path::new(source), Path::new(destination));
        if command == "pack" {
            return pack(src, dest, atomic_write_new);
        }
        if command == "checkout" {
            println!("{}", checkout(src, dest)?);
            return Ok(());
        }
        if command == "unpack" {
            return unpack(src, dest);
        }
    }
    if let [command, folder, expected, source] = args.as_slice()
        && command == "save"
        && let Some(expected) = expected.to_str()
    {
        println!("{}", save(Path::new(folder), expected, Path::new(source))?);
        return Ok(());
    }
    if let [command, folder] = args.as_slice()
        && command == "history"
    {
        return history(Path::new(folder), |number, hash| {
            use std::io::Write;
            writeln!(io::stdout().lock(), "{number} {hash}")
        });
    }
    if let [command, folder, revision] = args.as_slice()
        && command == "list-bins"
        && let Some(revision) = revision.to_str()
    {
        return list_bins(Path::new(folder), revision, io::stdout().lock());
    }
    if let [command, folder, revision, id] = args.as_slice()
        && command == "preview-bin"
        && let (Some(revision), Some(bin)) = (revision.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        use std::io::Write;
        let preview = preview_bin(Path::new(folder), revision, bin)?;
        let mut out = io::stdout().lock();
        serde_json::to_writer(&mut out, &preview)?;
        return writeln!(out);
    }
    if let [command, folder, revision, id] = args.as_slice()
        && command == "find-uses"
        && let (Some(revision), Some(item)) = (revision.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        use std::io::Write;
        let usage = converter::usage::find_uses(Path::new(folder), revision, item)?;
        let mut out = io::stdout().lock();
        serde_json::to_writer(&mut out, &usage)?;
        writeln!(out)?;
        return if usage.complete { Ok(()) } else { Err(io::Error::other("incomplete item usage search")) };
    }
    if let [command, folder, from, to, id] = args.as_slice()
        && (command == "compare-bin" || command == "compare-sequence")
        && let (Some(from), Some(to), Some(id)) = (from.to_str(), to.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        let selection = if command == "compare-bin" { Selection::Bin(id) } else { Selection::Sequence(id) };
        return compare(Path::new(folder), from, to, selection, io::stdout().lock());
    }
    if let [command, folder, base, revision, id] = args.as_slice()
        && command == "restore-copy"
        && let (Some(base), Some(revision), Some(id)) = (base.to_str(), revision.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        let (new_id, digest) = restore_copy(Path::new(folder), base, revision, id)?;
        println!("{new_id} {digest}");
        return Ok(());
    }
    if let [command, folder, revision, id, out] = args.as_slice()
        && (command == "rescue" || command == "rescue-bin")
        && let (Some(revision), Some(id)) = (revision.to_str(), id.to_str().and_then(|s| s.parse().ok()))
    {
        if command == "rescue-bin" {
            return rescue_bin(Path::new(folder), revision, id, Path::new(out));
        }
        return rescue(Path::new(folder), revision, id, Path::new(out));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: project_folder pack SOURCE.fcproj NEW_FOLDER | unpack FOLDER NEW_FILE.fcproj | checkout FOLDER NEW_FILE.fcproj | save FOLDER BASE_REVISION EDITED.fcproj | history FOLDER | compare-bin FOLDER FROM TO BIN_ID | compare-sequence FOLDER FROM TO SEQUENCE_ID | restore-copy FOLDER BASE_REVISION HISTORICAL_REVISION SEQUENCE_ID | rescue FOLDER REVISION SEQUENCE_ID NEW_FILE.fcproj | list-bins FOLDER REVISION | find-uses FOLDER REVISION ITEM_ID | preview-bin FOLDER REVISION BIN_ID | rescue-bin FOLDER REVISION BIN_ID NEW.fcproj",
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
