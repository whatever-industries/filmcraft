//! Import an original generated still, move its folder, then exercise real bin decisions.
use super::*;
use egui::accesskit::Role;

struct Source {
    f: Folder,
    id: ItemId,
    moved: PathBuf,
    online: Vec<u8>,
}

fn picture(s: &Session) -> Vec<u8> {
    s.render_program_at(1.0, Tick::ZERO).unwrap().over_black_rgba8()
}

impl Source {
    fn new(dir: &Path) -> Self {
        Self::with_pending_anchor(dir, false)
    }
    fn with_pending_anchor(dir: &Path, pending: bool) -> Self {
        let f = Folder::create(dir);
        let mut s = open(&f.path);
        let original = dir.join("Originals");
        fs::create_dir(&original).unwrap();
        let image = original.join("camera.png");
        image::RgbImage::from_pixel(160, 90, image::Rgb([236u8, 24, 58])).save(&image).unwrap();
        let imported = s.execute("file.import", json!({"paths":[image],"bin":f.selects})).unwrap();
        let id = ItemId(imported["items"][0].as_u64().unwrap());
        s.edit("Place source", |p, _| {
            p.sequence_mut(f.sequence).unwrap().video_tracks[0].items[0].item = id;
            if !pending {
                p.resolve_placed_auto_points(|_| true, |_| true);
            }
            Ok(())
        })
        .unwrap();
        s.execute("sequence.open", json!({"item":f.master})).unwrap();
        s.execute("file.save", json!({})).unwrap();
        let online = picture(&s);
        assert!(online.as_chunks::<4>().0.iter().any(|px| px[0] > 200 && px[2] < 80));
        fs::rename(original, dir.join("Moved")).unwrap();
        Self { f, id, moved: dir.join("Moved/camera.png"), online }
    }
    fn discover(&self, s: &mut Session) -> Value {
        let report = s.execute("media.search", json!({"folder":self.moved.parent(),"item":self.id})).unwrap();
        let r = report["results"][0]["candidates"][0]["review"].clone();
        assert_eq!(r["bin"], json!(self.f.selects));
        assert_eq!(r["revision"], report["revision"]);
        r
    }
    fn args(&self, review: &Value) -> Value {
        json!({"item":self.id,"bin":self.f.selects,"path":self.moved,"review":review})
    }
    fn apply(&self, s: &mut Session, review: &Value) {
        s.execute("project.select", json!({"items":[self.id]})).unwrap();
        s.execute("media.relink", self.args(review)).unwrap();
    }
}

fn apply_from_search(d: &mut Driver, src: &Source, floating: bool, before: &[u8]) {
    if floating {
        invoke(d, "openBin", json!({"bin":src.f.selects,"how":"window"}));
        let result = d.exec("edit.find", json!({"scope":"project","column":"Name","text":"camera"}));
        assert_eq!(result["item"], json!(src.id));
    }
    d.ok("ui.menu.invoke", json!({"id":"media.linkMedia"}));
    if floating {
        assert_eq!(d.app().ui.link_media.as_ref().unwrap().source_item, Some(src.id.0));
    }
    d.app().ui.link_media.as_mut().unwrap().folder = src.moved.parent().unwrap().to_string_lossy().into();
    d.frames(3);
    d.click("linkMedia.search");
    d.frames(2);
    d.click("linkMedia.link");
    d.frames(3);
    assert_eq!(d.app().session.state.project_selection, vec![src.id]);
    assert_eq!(d.app().ui.project_panel.tabs.iter().filter(|t| t.bin == src.f.selects.0).count(), 1);
    if floating {
        assert!(d.app().ui.project_panel.tabs.iter().any(|t| t.bin == src.f.selects.0 && t.floating));
    }
    assert!(picture(&d.app().session) == before);
    let _ = d.harness.get_by_role_and_label(Role::Dialog, "Review Source Relink");
    assert!(d.harness.get_by_role_and_label(Role::Button, "Back").is_focused());
    d.ok("ui.key", json!({"key":"Tab"}));
    assert!(d.harness.get_by_role_and_label(Role::Button, "Apply Relink").is_focused());
    d.ok("ui.key", json!({"key":"Enter"}));
    d.frames(3);
}

#[test]
fn project_wide_search_opens_the_owner_and_save_or_discard_restores_picture() {
    for save in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let src = Source::new(tmp.path());
        let mut d = src.f.driver();
        let before = picture(&d.app().session);
        assert_ne!(before, src.online);
        d.exec("clip.rename", json!({"item":src.f.master,"name":"Today's cut"}));
        apply_from_search(&mut d, &src, save, &before);
        assert!(picture(&d.app().session) == src.online);
        d.exec("edit.undo", json!({}));
        assert!(picture(&d.app().session) == before);
        assert!(d.app().session.offline.missing.contains(&src.id));
        assert_eq!(d.app().session.dirty_folder_bins(), [src.f.cuts].into());
        d.exec("edit.redo", json!({}));
        assert!(picture(&d.app().session) == src.online);
        assert!(!d.app().session.offline.missing.contains(&src.id));
        assert_eq!(d.app().session.dirty_folder_bins(), [src.f.cuts, src.f.selects].into());
        invoke(&mut d, "closeBinTab", json!({}));
        d.harness.get_by_role_and_label(Role::Button, if save { "Save" } else { "Discard" }).click();
        d.frames(3);
        let s = &mut d.app().session;
        assert_eq!(s.dirty_folder_bins(), [src.f.cuts].into());
        assert_eq!(s.project.item(src.f.master).unwrap().name, "Today's cut");
        assert_eq!(s.offline.missing.contains(&src.id), !save);
        let expected = if save { &src.online } else { &before };
        assert!(&picture(s) == expected);
        let reopened = open(&src.f.path);
        assert!(&picture(&reopened) == expected);
        let media = reopened.project.item(src.id).unwrap().as_media().unwrap();
        let filmcraft_project::MediaRef::File { path } = &media.media else { panic!("expected file media") };
        let stored = reopened.media.follow(path).unwrap();
        assert_eq!(Path::new(&stored).parent().unwrap().file_name().unwrap(), if save { "Moved" } else { "Originals" });
        assert_ne!(reopened.project.item(src.f.master).unwrap().name, "Today's cut");
    }
}

#[test]
fn changed_project_or_candidate_leaves_working_edits_and_newest_save_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let src = Source::new(tmp.path());
    let mut s = open(&src.f.path);
    let review = src.discover(&mut s);
    s.execute("clip.rename", json!({"item":src.f.master,"name":"Unpublished"})).unwrap();
    s.execute("project.select", json!({"items":[src.id]})).unwrap();
    let working = s.project.clone();
    let wrong_bin = json!({"item":src.id,"bin":src.f.cuts,"path":src.moved,"review":review});
    assert!(s.execute("media.relink", wrong_bin).is_err());
    let bytes = fs::read(&src.moved).unwrap();
    image::RgbImage::from_pixel(160, 90, image::Rgb([24u8, 44, 88])).save(&src.moved).unwrap();
    assert!(s.execute("media.relink", src.args(&review)).unwrap_err().to_string().contains("fingerprint"));
    fs::write(&src.moved, bytes).unwrap();
    let mut other = open(&src.f.path);
    other.execute("clip.rename", json!({"item":src.f.master,"name":"New saved cut"})).unwrap();
    other.execute("file.save", json!({})).unwrap();
    assert!(s.execute("media.relink", src.args(&review)).unwrap_err().to_string().contains("review again"));
    assert!(std::sync::Arc::ptr_eq(&s.project, &working));
    let mut current = open(&src.f.path);
    let review = src.discover(&mut current);
    src.apply(&mut current, &review);
    assert_eq!(current.dirty_folder_bins(), [src.f.selects].into());
    other.execute("clip.rename", json!({"item":src.f.master,"name":"Newest cut"})).unwrap();
    other.execute("file.save", json!({})).unwrap();
    let newest = fs::read(&src.f.path).unwrap();
    for command in ["project.saveBin", "project.discardBin"] {
        assert!(current.execute(command, json!({"bin":src.f.selects})).is_err());
        assert_eq!(current.dirty_folder_bins(), [src.f.selects].into());
        assert!(picture(&current) == src.online);
        assert_eq!(fs::read(&src.f.path).unwrap(), newest);
    }
}

#[test]
fn global_save_preserves_the_relink_unrelated_edits_and_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let src = Source::new(tmp.path());
    let mut s = open(&src.f.path);
    let review = src.discover(&mut s);
    s.execute("clip.rename", json!({"item":src.f.master,"name":"Latest edit"})).unwrap();
    s.execute("file.projectSettings.general", json!({"titleSafe":[12,14]})).unwrap();
    src.apply(&mut s, &review);
    s.execute("file.save", json!({})).unwrap();
    let disk = open(&src.f.path);
    assert!(picture(&disk) == src.online);
    assert_eq!(disk.project.item(src.f.master).unwrap().name, "Latest edit");
    assert_eq!(disk.project.settings, s.project.settings);
    assert!(!s.is_dirty());
}

#[test]
fn an_unresolved_sequence_anchor_requires_the_full_edit_and_preserves_the_reviewed_state() {
    let tmp = tempfile::tempdir().unwrap();
    let src = Source::with_pending_anchor(tmp.path(), true);
    let mut s = open(&src.f.path);
    let review = src.discover(&mut s);
    s.execute("project.select", json!({"items":[src.id]})).unwrap();
    let before = s.project.clone();
    let offline = picture(&s);
    let rev = s.revision;
    let result = s.execute("media.relink", src.args(&review)).unwrap_err().to_string();
    assert!(result.contains("Interview") && result.contains("full editor"), "{result}");
    assert!(std::sync::Arc::ptr_eq(&before, &s.project));
    assert_eq!(s.revision, rev);
    assert!(picture(&s) == offline);
    assert!(s.dirty_folder_bins().is_empty());
    assert!(s.undo().is_none());
    let full = json!({"fullEditorReview":review,"item":src.id,"bin":src.f.selects,"path":src.moved});
    s.execute("media.relink", full).unwrap();
    assert!(picture(&s) == src.online);
    assert_ne!(s.project.sequence(src.f.sequence), before.sequence(src.f.sequence));
}

#[test]
fn review_requires_direct_unique_saved_ownership_and_a_recorded_fingerprint() {
    let tmp = tempfile::tempdir().unwrap();
    let src = Source::new(tmp.path());
    let mut s = open(&src.f.path);
    let review = src.discover(&mut s);
    let rev = s.folder_revision().unwrap().to_owned();
    let locate = json!({"reviewOnly":true,"revision":rev,"item":src.id,"path":src.moved});
    assert_eq!(s.execute("media.relink", locate.clone()).unwrap()["review"], review);
    assert!(!s.is_dirty());
    s.execute("project.select", json!({"items":[src.f.sequence]})).unwrap();
    assert!(s.execute("media.relink", src.args(&review)).is_err());
    s.execute("project.select", json!({"items":[src.id]})).unwrap();
    for variation in ["moved", "unfiled", "ambiguous", "no fingerprint"] {
        s.edit("Change source eligibility", |p, _| {
            match variation {
                "moved" => {
                    p.root.remove_item(src.id);
                    p.root.find_bin_mut(src.f.cuts).unwrap().children.push(filmcraft_project::BinEntry::Item(src.id));
                }
                "unfiled" => {
                    p.root.remove_item(src.id);
                }
                "ambiguous" => {
                    p.root.find_bin_mut(src.f.cuts).unwrap().children.push(filmcraft_project::BinEntry::Item(src.id));
                }
                _ => {
                    p.item_mut(src.id).unwrap().as_media_mut().unwrap().identity = None;
                }
            }
            Ok(())
        })
        .unwrap();
        let before = s.project.clone();
        assert!(s.execute("media.relink", src.args(&review)).is_err(), "{variation}");
        assert!(s.execute("media.relink", locate.clone()).is_err(), "{variation}");
        assert!(std::sync::Arc::ptr_eq(&before, &s.project));
        s.undo().unwrap();
    }
}

/// An inactive tab retains its name and order while its timeline stays on disk.
fn warm_source(src: &Source, data: &Path) -> (Session, ItemId) {
    use filmcraft_format::project_folder::{
        FolderDirectory,
        index::{Index, cache_directory},
    };
    let mut full = open(&src.f.path);
    let deferred = full
        .edit("Archive cut", |p, _| {
            let bin = p.add_bin("Archive", None);
            Ok(p.new_sequence("Archive reel", Default::default(), 1, 0, Some(bin)))
        })
        .unwrap();
    full.execute("sequence.open", json!({"item":deferred})).unwrap();
    full.execute("sequence.open", json!({"item":src.f.master})).unwrap();
    full.execute("file.save", json!({})).unwrap();
    let mut s = Session::local_folder_editor();
    s.enable_experimental_modular_relink(data.to_owned());
    s.execute("file.open", json!({"path":src.f.path})).unwrap();
    assert!(!s.has_deferred_timelines(), "a cold cache opens the complete editor");
    let directory = FolderDirectory::open(&src.f.path).unwrap();
    let (index, ..) = Index::build(&directory, None).unwrap();
    fs::write(cache_directory(&src.f.path, data).unwrap().join("index.json"), index.bytes().unwrap()).unwrap();
    s.execute("file.open", json!({"path":src.f.path})).unwrap();
    assert!(s.has_deferred_timelines());
    assert!(s.project.item(deferred).is_none());
    assert_eq!(s.project_view(), full.project_view());
    assert_eq!(s.sequence_name(deferred), Some("Archive reel"));
    (s, deferred)
}

#[test]
fn warm_relink_promotes_in_place_and_bin_close_preserves_other_edits() {
    for save in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let src = Source::new(tmp.path());
        let (s, deferred) = warm_source(&src, &tmp.path().join("data"));
        let mut d = Driver::open(s, support::with_shots(support::builder()));
        let offline = picture(&d.app().session);
        let before = fs::read(&src.f.path).unwrap();
        apply_from_search(&mut d, &src, true, &offline);
        let s = &mut d.app().session;
        assert!(s.has_deferred_timelines(), "the desktop promoted during source-only review");
        assert_eq!(s.dirty_folder_bins(), [src.f.selects].into());
        assert!(picture(s) == src.online);
        s.execute("edit.undo", json!({})).unwrap();
        assert!(picture(s) == offline);
        let view = s.project_view();
        let selection = s.state.project_selection.clone();
        // A general query hydrates the full model while the relink lives on Redo.
        s.execute("file.projectSettings.general", json!({})).unwrap();
        assert!(!s.has_deferred_timelines());
        assert!(s.project.sequence(deferred).is_some());
        assert_eq!(s.project_view(), view);
        assert_eq!(s.state.project_selection, selection);
        s.execute("edit.redo", json!({})).unwrap();
        assert!(picture(s) == src.online);
        s.execute("clip.rename", json!({"item":src.f.master,"name":"Today's cut"})).unwrap();
        s.execute("file.projectSettings.general", json!({"titleSafe":[12,14]})).unwrap();
        assert_eq!(fs::read(&src.f.path).unwrap(), before);
        invoke(&mut d, "closeBinTab", json!({}));
        d.harness.get_by_role_and_label(Role::Button, if save { "Save" } else { "Discard" }).click();
        d.frames(3);
        let s = &mut d.app().session;
        let disk = open(&src.f.path);
        assert_eq!(picture(&disk) == src.online, save);
        assert_ne!(disk.project.item(src.f.master).unwrap().name, "Today's cut");
        assert_ne!(disk.project.settings, s.project.settings);
        assert_eq!(s.dirty_folder_bins(), [src.f.cuts].into());
        assert!(!s.state.open_sequences.contains(&src.f.sequence));
        s.execute("file.save", json!({})).unwrap();
        assert_eq!(open(&src.f.path).project, s.project);
        assert_eq!(s.execute("file.save", json!({})).unwrap()["bytes"], 0);
    }
}

#[test]
fn failed_promotion_preserves_the_working_relink_for_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let src = Source::new(tmp.path());
    let (mut s, deferred) = warm_source(&src, &tmp.path().join("data"));
    let review = src.discover(&mut s);
    src.apply(&mut s, &review);
    let revision = s.revision;
    let manifest: Value = serde_json::from_slice(&fs::read(&src.f.path).unwrap()).unwrap();
    let key = manifest["items"].as_array().unwrap().iter().find(|row| row[0] == deferred.0).unwrap()[1].as_str().unwrap();
    let record = src.f.path.with_file_name(key);
    let moved = tmp.path().join("missing-record");
    fs::rename(&record, &moved).unwrap();
    let rename = json!({"item":src.f.master,"name":"Today's cut"});
    assert!(s.execute("clip.rename", rename.clone()).is_err());
    assert!(s.has_deferred_timelines());
    assert_eq!(s.revision, revision);
    assert!(picture(&s) == src.online);
    let press = s.undo_mark();
    fs::rename(moved, record).unwrap();
    s.execute("clip.rename", rename).unwrap();
    assert_eq!(s.revert_to(press), 1);
    assert!(picture(&s) == src.online);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.is_dirty());
    assert!(picture(&s) != src.online);
}

#[path = "verified_relink_crash.rs"]
mod crash;

#[test]
fn source_bin_decision_leaves_deferred_cut_on_disk() {
    for save in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let src = Source::new(tmp.path());
        let (s, deferred) = warm_source(&src, &tmp.path().join("data"));
        let baseline = open(&src.f.path);
        let mut d = Driver::open(s, support::with_shots(support::builder()));
        let offline = picture(&d.app().session);
        apply_from_search(&mut d, &src, true, &offline);
        invoke(&mut d, "closeBinTab", json!({}));
        d.harness.get_by_role_and_label(Role::Button, if save { "Save" } else { "Discard" }).click();
        d.frames(3);
        let s = &mut d.app().session;
        assert!(s.has_deferred_timelines());
        assert!(s.project.item(deferred).is_none());
        assert!(!s.is_dirty());
        let disk = open(&src.f.path);
        assert_eq!(picture(&disk) == src.online, save);
        assert_eq!(disk.project.item(deferred), baseline.project.item(deferred));
        assert_eq!(disk.project.settings, baseline.project.settings);
        assert_eq!(s.execute("project.saveBin", json!({"bin":src.f.selects})).unwrap()["bytes"], 0);
    }
}

#[test]
fn warm_apply_rechecks_the_published_revision_before_changing_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let src = Source::new(tmp.path());
    let data = tmp.path().join("data");
    let (mut s, _) = warm_source(&src, &data);
    let review = src.discover(&mut s);
    let mut writer = open(&src.f.path);
    writer.execute("clip.rename", json!({"item":src.f.master,"name":"New published cut"})).unwrap();
    writer.execute("project.saveBin", json!({"bin":src.f.cuts})).unwrap();
    s.execute("project.select", json!({"items":[src.id]})).unwrap();
    assert!(s.execute("media.relink", src.args(&review)).is_err());
    assert!(!s.is_dirty());
    s.execute("file.open", json!({"path":src.f.path})).unwrap();
    assert!(!s.has_deferred_timelines());
    assert_eq!(s.project.item(src.f.master).unwrap().name, "New published cut");
}
