//! Measure real editor open and first bin navigation on a standard project and its modular counterpart.
//! See "Project open and first bin activation" in docs/modular-project-rules.md.
#[path = "preview.rs"]
mod preview;
#[path = "reader.rs"]
mod reader;
use filmcraft_engine::Session;
use filmcraft_format::{MAX_PROJECT_BYTES, atomic_write_new, decode, encode_with_view, project_folder};
use filmcraft_media::reader::read_regular_file_bounded;
use filmcraft_project::{Bin, BinEntry, BinId, ItemId, Project};
use filmcraft_ui_egui::{
    FilmcraftApp,
    panels::project::{Inst, open_bin, shown_inst, visible_items},
};
use serde_json::json;
use std::{collections::BTreeSet, error::Error, fs, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn direct(bin: &Bin) -> impl Iterator<Item = ItemId> + '_ {
    bin.children.iter().filter_map(|entry| match entry {
        BinEntry::Item(id) => Some(*id),
        BinEntry::Bin(_) => None,
    })
}

fn include_sources(project: &Project, seeds: impl IntoIterator<Item = ItemId>) -> Result<BTreeSet<ItemId>> {
    let mut resident = BTreeSet::new();
    let mut pending: Vec<_> = seeds.into_iter().collect();
    while let Some(id) = pending.pop() {
        if resident.insert(id) {
            let item = project.item(id).ok_or_else(|| format!("missing source {}", id.0))?;
            item.visit_source_items(|source| pending.push(source));
        }
    }
    Ok(resident)
}

fn prepare(source: &Path, destination: &Path) -> Result<()> {
    let loaded = decode(&read_regular_file_bounded(source, MAX_PROJECT_BYTES)?)?;
    let view = loaded.view.ok_or("the fixture needs a saved timeline view")?;
    view.active_sequence.ok_or("the fixture needs an active timeline")?;
    fs::create_dir(destination)?;
    let standard = destination.join("project.fcproj");
    let model = project_folder::detached_project(loaded.project, source, Some(&standard))?;
    atomic_write_new(&standard, &encode_with_view(&model, Some(&view), false))?;
    project_folder::pack(&standard, &destination.join("folder"), atomic_write_new)?;
    Ok(())
}

fn measure(path: &Path, bin: BinId) -> Result<()> {
    let mut app = FilmcraftApp::new(Session::local_folder_editor());
    let start = Instant::now();
    app.session.execute("file.open", json!({"path":path}))?;
    let open_ms = start.elapsed().as_secs_f64() * 1000.0;
    let model = app.session.project.clone();
    let active = app.session.state.active_sequence;
    let start = Instant::now();
    open_bin(&mut app, Inst::Main, bin.0, Some("openInPlace"), egui::Modifiers::NONE)?;
    let visible = visible_items(&app, shown_inst(&app));
    let first_us = start.elapsed().as_secs_f64() * 1e6;
    if !std::sync::Arc::ptr_eq(&model, &app.session.project) || app.session.is_dirty() || app.session.state.active_sequence != active {
        return Err("opening a bin changed the working model or active cut".into());
    }
    println!(
        "{}",
        json!({"items":model.items.len(),"visibleItems":visible.len(),"tabs":app.session.state.open_sequences.len(),"active":active,"openMs":open_ms,"firstBinUs":first_us})
    );
    Ok(())
}

fn selective(path: &Path, bin: BinId, all_tabs: bool) -> Result<()> {
    let start = Instant::now();
    let mut reader = reader::Reader::open(path)?;
    reader.startup(all_tabs)?;
    let open_ms = start.elapsed().as_secs_f64() * 1000.0;
    let open_bytes = reader.bytes;
    let open_items = reader.items.len();
    let start = Instant::now();
    reader.activate(bin)?;
    let first_us = start.elapsed().as_secs_f64() * 1e6;
    let end_bytes = reader.bytes;
    reader.activate(bin)?;
    if reader.bytes != end_bytes {
        return Err("repeat bin activation reread records".into());
    }
    println!(
        "{}",
        json!({"openMs":open_ms,"openBytes":open_bytes,"openItems":open_items,"firstBinUs":first_us,"newItems":reader.items.len()-open_items,"newBytes":end_bytes-open_bytes})
    );
    Ok(())
}

fn verify(full: &Path, bin: BinId) -> Result<serde_json::Value> {
    let mut original = Session::local_folder_editor();
    original.execute("file.open", json!({"path":full}))?;
    let mut read = reader::Reader::open(full)?;
    read.startup(false)?;
    if read.view.active_sequence != original.state.active_sequence || read.view.open_sequences != original.state.open_sequences {
        return Err("saved timeline tabs differ from the editor".into());
    }
    // Supply the reference session's global settings to the renderer. Only the experiment's
    // resident item records are available to resolve media and nested timeline references.
    let mut projected = Session::default();
    let mut model = (*original.project).clone();
    model.items = read.items.clone();
    projected.project = std::sync::Arc::new(model);
    projected.path = original.path.clone();
    projected.state = original.state.clone();
    projected.media.add_stored(&projected.project, projected.path.as_deref());
    let active = original.state.active_sequence.ok_or("missing active sequence")?;
    let needed = include_sources(&original.project, [active])?;
    for id in &needed {
        let item = projected.project.item(*id).ok_or("projection lost a playback dependency")?;
        if Some(item) != original.project.item(*id) {
            return Err("resident item differs from full open".into());
        }
    }
    let tick = original.sequence_rate().tick_of(7);
    let sample = |s: &mut Session| -> Result<_> {
        s.set_playhead(tick);
        let frame = s.render_program(0.25).ok_or("no program frame")?.over_black_rgba8();
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        let pcm = filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().ok_or("no sequence")?, 14000, 240, &provider).channels;
        Ok((frame, pcm))
    };
    let (frame, pcm) = sample(&mut original)?;
    let (other, sound) = sample(&mut projected)?;
    if frame != other || pcm != sound {
        return Err("active timeline picture or sound differs".into());
    }
    let first = original.project.root.find_bin(bin).ok_or("unknown bin")?;
    let needed_after = include_sources(&original.project, direct(first).chain(direct(&original.project.root)).chain([active]))?;
    read.activate(bin)?;
    if needed_after != read.items.keys().copied().collect() || read.items.iter().any(|(id, item)| original.project.item(*id) != Some(item)) {
        return Err("activated bin differs from full open".into());
    }
    let (pixels, _) = frame.as_chunks::<4>();
    let picture_varies = pixels.first().is_some_and(|first| pixels.iter().any(|pixel| pixel != first));
    Ok(json!({"dependenciesIncludingSequence":needed.len(),"frameBytes":frame.len(),"audioChannels":pcm.len(),
        "nonSilent":pcm.iter().flatten().any(|v|v.abs()>0.00001),"pictureVaries":picture_varies}))
}

pub(super) fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, path, old, out] if mode == "refresh" => preview::refresh(Path::new(path), Some(Path::new(old)), Path::new(out)),
        [mode, path, out] if mode == "index" => preview::index(Path::new(path), Path::new(out)),
        [mode, path] if mode == "preview" => preview::window(Path::new(path), None),
        [mode, path, index] if mode == "preview" => preview::window(Path::new(path), Some(Path::new(index))),
        [mode, path, bin] if mode == "frames" => preview::measure_frames(Path::new(path), None, BinId(bin.parse()?)),
        [mode, path, bin, index] if mode == "frames" => preview::measure_frames(Path::new(path), Some(Path::new(index)), BinId(bin.parse()?)),
        [mode, source, destination] if mode == "prepare" => prepare(Path::new(source), Path::new(destination)),
        [mode, path, bin] if mode == "measure" => measure(Path::new(path), BinId(bin.parse()?)),
        [mode, full, bin] if mode == "verify" => {
            println!("{}", verify(Path::new(full), BinId(bin.parse()?))?);
            Ok(())
        }
        [mode, path, bin] if mode == "active" || mode == "all-tabs" => selective(Path::new(path), BinId(bin.parse()?), mode == "all-tabs"),
        _ => Err(concat!(
            "expected prepare SOURCE.fcproj NEW_DIRECTORY, measure PROJECT BIN_ID, ",
            "active|all-tabs|verify FOLDER_MANIFEST BIN_ID, index FOLDER_MANIFEST INDEX, preview FOLDER_MANIFEST [INDEX], ",
            "refresh FOLDER_MANIFEST PRIOR_INDEX NEW_INDEX, or frames PROJECT BIN_ID [INDEX]"
        )
        .into()),
    }
}
