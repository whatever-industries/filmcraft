use super::*;
use crate::native::preview::relink_fixture::{Case, cut};
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use std::{fs, sync::Arc};

#[test]
fn predicted_records_and_bins_match_a_real_edit_on_the_same_revision() -> Result<()> {
    for case in [Case::Plain, Case::Dependent, Case::Marked] {
        let temp = tempfile::tempdir()?;
        let f = cut(temp.path(), case)?;
        let manifest = fs::read(&f.manifest)?;
        let predicted = check(&f.manifest, &temp.path().join("cache"), &f.review, || Ok(()))?;
        let result = &predicted["results"];
        let changed: BTreeSet<ItemId> = serde_json::from_value(result["changed"].clone())?;
        match case {
            Case::Plain => {
                assert_eq!(result["status"], "source-only");
                assert_eq!(changed, [f.review.item].into());
            }
            Case::Dependent => {
                assert_eq!(result["status"], "full-editor");
                assert_eq!(changed, [f.review.item, f.nested, f.other, f.script].into());
                assert_eq!(result["unfiled"], json!([f.script]));
                assert_eq!(result["owners"][f.other.0.to_string()].as_array().unwrap().len(), 2);
            }
            Case::Marked => {
                assert_eq!(result["status"], "script-review");
                assert_eq!(result["scriptReview"], json!([f.script]));
                assert!(changed.is_empty());
            }
        }
        assert!(!changed.contains(&f.master));
        let mut open = Session::local_folder_editor();
        open.execute("file.open", json!({"path":f.manifest}))?;
        let before = open.project.clone();
        let missing = open.offline.missing.clone();
        let view = open.project_view();
        let picture = open.try_render_program_at(0.25, Tick::ZERO)?.over_black_rgba8();
        assert_eq!(review::impact(&open, &f.review.revision, &f.review)?, *result);
        assert!(Arc::ptr_eq(&before, &open.project));
        assert_eq!(open.offline.missing, missing);
        assert_eq!(open.project_view(), view);
        assert_eq!(open.try_render_program_at(0.25, Tick::ZERO)?.over_black_rgba8(), picture);
        assert!(!open.history.can_undo() && !open.is_dirty() && open.dirty_folder_bins().is_empty());
        compare_editor(&f.manifest, &f.review, result)?;
        assert_eq!(fs::read(&f.manifest)?, manifest);
    }
    Ok(())
}

#[test]
fn interrupted_or_incomplete_checks_cannot_return_a_source_only_prediction() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let f = cut(temp.path(), Case::Dependent)?;
    let cache = temp.path().join("cache");
    let original = fs::read(&f.review.path)?;
    let replaced = check(&f.manifest, &cache, &f.review, || {
        fs::write(&f.review.path, b"replaced")?;
        Ok(())
    });
    assert!(replaced.unwrap_err().to_string().contains("candidate changed"));
    fs::write(&f.review.path, original)?;
    let manifest: Value = serde_json::from_slice(&fs::read(&f.manifest)?)?;
    let records: Vec<(ItemId, String)> = serde_json::from_value(manifest["items"].clone())?;
    let hash = &records.iter().find(|(id, _)| *id == f.script).unwrap().1;
    let script = f.manifest.with_file_name(hash);
    let bytes = fs::read(&script)?;
    fs::remove_file(&script)?;
    assert!(check(&f.manifest, &cache, &f.review, || Ok(())).unwrap_err().to_string().contains("could not read item"));
    fs::write(script, bytes)?;
    let saved = check(&f.manifest, &cache, &f.review, || {
        let mut editor = Session::local_folder_editor();
        editor.execute("file.open", json!({"path":f.manifest}))?;
        editor.execute("clip.rename", json!({"item":f.master,"name":"Today's master"}))?;
        editor.execute("file.save", json!({}))?;
        Ok(())
    });
    assert!(saved.unwrap_err().to_string().contains("project changed during impact"));
    assert!(check(&f.manifest, &cache, &f.review, || Ok(())).unwrap_err().to_string().contains("project changed"));
    Ok(())
}
