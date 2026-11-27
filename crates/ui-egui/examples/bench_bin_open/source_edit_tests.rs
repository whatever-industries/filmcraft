use super::*;
use crate::native::preview::{
    impact,
    relink_fixture::{Case, Cut, cut},
};
use filmcraft_format::project_folder;
use filmcraft_time::Tick;
use serde_json::Value;
use std::fs;

fn warm(f: &Cut, data: &Path) -> Result<SourceEdit> {
    impact::check(&f.manifest, data, &f.review, || Ok(()))?;
    SourceEdit::open(&f.manifest, data, f.review.clone())
}

fn open(path: &Path) -> Result<Session> {
    let mut editor = Session::local_folder_editor();
    editor.execute("file.open", json!({"path":path}))?;
    Ok(editor)
}

fn picture(s: &Session) -> Result<Vec<u8>> {
    Ok(s.try_render_program_at(0.25, Tick::ZERO)?.over_black_rgba8())
}

fn raw(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

#[test]
fn partial_relink_matches_editor_through_history_save_discard_and_recovery() -> Result<()> {
    for finish in ["save", "discard"] {
        let temp = tempfile::tempdir()?;
        let f = cut(temp.path(), Case::Plain)?;
        let folder = f.manifest.parent().unwrap();
        let snapshot = raw(&f.manifest)?;
        let data = temp.path().join("cache");
        assert!(SourceEdit::open(&f.manifest, &data, f.review.clone()).err().unwrap().to_string().contains("warm index"));
        let mut partial = warm(&f, &data)?;
        // Load only the nested cut being viewed and its sources, like the read-only renderer.
        let playback = partial.view.reader.missing(vec![f.nested], &partial.view.session.project.items)?;
        Arc::make_mut(&mut partial.view.session.project).items.extend(playback);
        partial.view.session.state.active_sequence = Some(f.nested);
        partial.view.session.media.add_stored(&partial.view.session.project, partial.view.session.path.as_deref());
        let reference = temp.path().join("full");
        fs::create_dir(&reference)?;
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), reference.join(entry.file_name()))?;
            }
        }
        let reference = reference.join(project_folder::ROOT);
        let mut full = open(&reference)?;
        full.state.active_sequence = Some(f.nested);
        let initial = picture(&full)?;
        full.execute("project.select", json!({"items":[f.review.item]}))?;
        for action in ["apply", "undo", "redo"] {
            partial.action(action)?;
            match action {
                "apply" => {
                    full.execute("media.relink", json!({"item":f.review.item,"bin":f.review.bin,"path":f.review.path,"review":f.review}))?;
                }
                "undo" => {
                    full.execute("edit.undo", json!({}))?;
                }
                _ => {
                    full.execute("edit.redo", json!({}))?;
                }
            }
            assert_eq!(picture(&partial.view.session)?, picture(&full)?);
            assert_eq!(picture(&full)? == initial, action == "undo");
            assert_eq!(partial.source()?, full.project.item(f.review.item).unwrap());
            assert_eq!(partial.view.session.history.undo.len(), full.history.undo.len());
            assert_eq!(partial.view.session.offline.missing.contains(&f.review.item), full.offline.missing.contains(&f.review.item));
            assert_eq!(partial.dirty(), full.dirty_folder_bins().contains(&f.review.bin));
            assert_eq!(partial.bytes_written, 0);
            assert_eq!(raw(&f.manifest)?, snapshot);
        }
        let online = picture(&full)?;
        partial.action(finish)?;
        full.execute(if finish == "save" { "project.saveBin" } else { "project.discardBin" }, json!({"bin":f.review.bin}))?;
        assert!(full.dirty_folder_bins().is_empty());
        let reference_reopened = open(&reference)?;
        assert!(!partial.dirty());
        let mut reopened = open(&f.manifest)?;
        assert_eq!(reference_reopened.project, reopened.project);
        assert_eq!(reference_reopened.project_view(), reopened.project_view());
        reopened.state.active_sequence = Some(f.nested);
        let expected = if finish == "save" { &online } else { &initial };
        assert_eq!(&picture(&partial.view.session)?, expected);
        assert_eq!(&picture(&reopened)?, expected);
        assert_eq!(reopened.project.item(f.review.item), Some(partial.source()?));
        let current = raw(&f.manifest)?;
        if finish == "discard" {
            assert_eq!(current, snapshot);
            assert!(!partial.view.session.history.can_redo());
            continue;
        }
        for key in ["envelope", "bins", "copies", "identities"] {
            assert_eq!(current[key], snapshot[key]);
        }
        let mut changed = Vec::new();
        for (old, new) in snapshot["items"].as_array().unwrap().iter().zip(current["items"].as_array().unwrap()) {
            if old != new {
                changed.push(old[0].clone());
                assert_eq!(raw(&folder.join(new[1].as_str().unwrap()))?["extraSourceField"], json!({"future":true}));
            }
        }
        assert_eq!(changed, vec![json!(f.review.item)]);
        assert_eq!(current["revision"]["backed_up_bins"], json!([f.review.bin]));
        let versions = project_folder::recovery_versions(folder, None)?;
        assert!(versions["versions"].as_array().unwrap().iter().any(|v| v["revision"] == f.review.revision));
        let rescue = temp.path().join("old.fcproj");
        project_folder::rescue(folder, &f.review.revision, f.nested.0, &rescue)?;
        let rescued = open(&rescue)?;
        let restored = rescued.project.item(f.review.item).unwrap().as_media().unwrap();
        assert_ne!(Some(restored), partial.source()?.as_media());
        let filmcraft_project::MediaRef::File { path } = &restored.media else { panic!("expected a file source") };
        assert!(Path::new(&rescued.media.follow(path).unwrap()).ends_with("Originals/camera.png"));
        assert_eq!(restored.identity, Some(f.review.identity));
        assert!(rescued.offline.missing.contains(&f.review.item));
        let written = partial.bytes_written;
        partial.save()?;
        assert_eq!(partial.bytes_written, written);
        assert_eq!(raw(&f.manifest)?, current);
    }
    Ok(())
}

#[test]
fn stale_edits_refuse_publication_and_failed_save_can_retry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = cut(temp.path(), Case::Plain)?;
    let cache = temp.path().join("cache");
    let mut partial = warm(&f, &cache)?;
    let mut stale = SourceEdit::open(&f.manifest, &cache, f.review.clone())?;
    partial.apply()?;
    let mut pending = SourceEdit::open(&f.manifest, &cache, f.review.clone())?;
    pending.apply()?;
    // A failed archive write occurs after staging the new immutable source, before replacing the head.
    let archive = f.manifest.with_file_name(&f.review.revision);
    fs::create_dir(&archive)?;
    assert!(partial.save().is_err());
    assert!(partial.dirty());
    let old = open(&f.manifest)?;
    assert!(old.offline.missing.contains(&f.review.item));
    fs::remove_dir(archive)?;
    partial.save()?;
    assert_ne!(partial.revision, f.review.revision);
    assert!(stale.apply().unwrap_err().to_string().contains("stale"));
    assert!(!stale.dirty());
    for action in ["save", "discard"] {
        assert!(pending.action(action).unwrap_err().to_string().contains("stale"));
        assert!(pending.dirty());
    }
    partial.action("undo")?;
    let working = partial.source()?.clone();
    let mut other = open(&f.manifest)?;
    other.execute("clip.rename", json!({"item":f.master,"name":"Newer master"}))?;
    other.execute("file.save", json!({}))?;
    let newer = raw(&f.manifest)?;
    for action in ["save", "discard"] {
        assert!(partial.action(action).unwrap_err().to_string().contains("stale"));
        assert!(partial.dirty());
        assert_eq!(partial.source()?, &working);
        assert_eq!(raw(&f.manifest)?, newer);
    }
    Ok(())
}

#[test]
fn dependent_marked_replaced_or_unreadable_source_never_applies() -> Result<()> {
    for case in [Case::Dependent, Case::Marked, Case::Plain] {
        let temp = tempfile::tempdir()?;
        let f = cut(temp.path(), case)?;
        let mut edit = warm(&f, &temp.path().join("cache"))?;
        let original = fs::read(&f.review.path)?;
        if matches!(case, Case::Plain) {
            fs::write(&f.review.path, b"different candidate")?;
        }
        let expected = match case {
            Case::Dependent => "full-editor",
            Case::Marked => "script-review",
            Case::Plain => "fingerprint",
        };
        assert!(edit.apply().unwrap_err().to_string().contains(expected));
        assert!(!edit.dirty() && !edit.view.session.history.can_undo());
        if matches!(case, Case::Plain) {
            fs::write(&f.review.path, &original)?;
            let reappeared = temp.path().join("Originals/camera.png");
            fs::create_dir(reappeared.parent().unwrap())?;
            fs::write(&reappeared, original)?;
            assert!(edit.apply().unwrap_err().to_string().contains("no longer a missing file"));
            fs::remove_file(reappeared)?;

            let manifest = raw(&f.manifest)?;
            let record = manifest["items"].as_array().unwrap().iter().find(|r| r[0] == f.review.item.0).unwrap();
            fs::remove_file(f.manifest.with_file_name(record[1].as_str().unwrap()))?;
            assert!(edit.apply().is_err());
            assert!(!edit.dirty() && !edit.view.session.history.can_undo());
        }
    }
    Ok(())
}
