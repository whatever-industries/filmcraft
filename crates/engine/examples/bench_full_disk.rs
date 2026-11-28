//! Local helper for disk-full trials. Prepare fixtures with `prepare ROOT [FCPROJ]`.
//! Start an editor with `run ROOT MANIFEST DATA`, or `recover ROOT MANIFEST DATA`
//! when the original volume is offline. Commands on stdin each return one JSON line.
//! The caller controls free space on the disposable project or recovery volume.
use filmcraft_engine::{
    Session,
    autosave::{AutosaveConfig, WorkerEvent},
};
use filmcraft_format::project_folder::{
    self, FolderDirectory,
    index::{Index, cache_directory},
};
use filmcraft_project::{ClipId, ItemId, ProjectView, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct RetryCounter(AtomicU64);
static COUNTER: RetryCounter = RetryCounter(AtomicU64::new(0));

impl log::Log for RetryCounter {
    fn enabled(&self, meta: &log::Metadata<'_>) -> bool {
        meta.target() == "filmcraft_engine::autosave::disk_full"
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn flush(&self) {}
}

fn prepare(root: &Path, input: Option<&str>) -> Result<()> {
    for output in ["seed.fcproj", "info.json", "project"] {
        if root.join(output).try_exists()? {
            return Err(format!("fixture output already exists: {output}").into());
        }
    }
    let mut s = Session::default();
    if let Some(path) = input {
        s.execute("file.open", json!({"path":path}))?;
    } else {
        let p = Arc::make_mut(&mut s.project);
        for name in ["Cuts", "Selects"] {
            let bin = p.add_bin(name, None);
            p.new_sequence(name, Default::default(), 1, 0, Some(bin));
        }
    }
    let reel = s.project.sequences().next().ok_or("need a reel")?.id;
    let color = s.execute("file.newColorMatte", json!({"color":"#e83369","width":160,"height":90,"seconds":4}))?;
    let p = Arc::make_mut(&mut s.project);
    let owner = p.root.parent_of(reel).ok_or("need an owning bin")?;
    let seq = p.sequence(reel).ok_or("missing reel")?;
    let rate = seq.settings.frame_rate;
    let start = rate.snap(seq.duration()) + rate.tick_of(1);
    let end = rate.tick_of(48);
    let matte = ItemId(color["item"].as_u64().ok_or("missing matte id")?);
    let clip = p.make_track_item(matte, TrackKind::Video, start, TimeRange::new(Tick::ZERO, end), rate).ok_or("could not place matte")?;
    let info = json!({"bin":owner,"reel":reel,"clip":clip.id,"sample":start+rate.tick_of(32)});
    let items = &mut p.sequence_mut(reel).and_then(|seq| seq.video_tracks.first_mut()).ok_or("reel has no video track")?.items;
    // Replace the last placement to keep the feature fixtures at their original scale.
    items.pop();
    items.push(clip);
    p.resolve_placed_auto_points(|_| true, |_| true);
    let view = ProjectView { open_sequences: vec![reel], active_sequence: Some(reel), ..Default::default() };
    let file = root.join("seed.fcproj");
    fs::write(&file, filmcraft_format::encode_with_view(p, Some(&view), false))?;
    fs::write(root.join("info.json"), serde_json::to_vec(&info)?)?;
    project_folder::pack(&file, &root.join("project"), filmcraft_format::atomic_write_new)?;
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let root = Path::new(args.get(1).ok_or("prepare ROOT [FCPROJ] or run ROOT MANIFEST DATA")?);
    if args[0] == "prepare" && args.len() <= 3 {
        return prepare(root, args.get(2).map(String::as_str));
    }
    if !matches!(args[0].as_str(), "run" | "recover") || args.len() != 4 {
        return Err("expected run|recover ROOT MANIFEST DATA".into());
    }
    log::set_logger(&COUNTER).map_err(|_| "logger already registered")?;
    log::set_max_level(log::LevelFilter::Debug);
    let manifest = Path::new(&args[2]);
    let data = Path::new(&args[3]);
    let info: Value = serde_json::from_slice(&fs::read(root.join("info.json"))?)?;
    let reel = ItemId(info["reel"].as_u64().ok_or("missing reel id")?);
    let clip = ClipId(info["clip"].as_u64().ok_or("missing clip id")?);
    let sample = Tick(info["sample"].as_i64().ok_or("missing sample time")?);
    let mut s = Session::local_folder_editor();
    if args[0] != "recover" {
        let (index, ..) = Index::build(&FolderDirectory::open(manifest)?, None)?;
        fs::write(cache_directory(manifest, data)?.join("index.json"), index.bytes()?)?;
        s.enable_experimental_modular_relink(data.to_owned());
        s.execute("file.open", json!({"path":manifest}))?;
    }
    s.start_autosave(AutosaveConfig::new(data))?;
    let mut prefs = s.prefs.clone();
    prefs.auto_save.enabled = false;
    s.set_prefs(prefs)?;
    println!("ready");
    for cmd in io::stdin().lock().lines() {
        let cmd = cmd?;
        let result = (|| -> Result<Value> {
            Ok(match cmd.as_str() {
                "recover" => s.execute("file.recover", json!({}))?,
                "trim" => s.execute("timeline.trim", json!({"clip":clip,"edge":"out","deltaFrames":-12}))?,
                "global-save" => s.execute("file.save", json!({}))?,
                "toggle-project-setting" => {
                    let display = if s.project.settings.audio_display_samples { "milliseconds" } else { "samples" };
                    s.execute("file.projectSettings.general", json!({"audioDisplay": display}))?
                }
                "save" => s.execute("project.saveBin", json!({"bin":info["bin"]}))?,
                "flush" => json!(s.persistence.as_ref().ok_or("missing recovery worker")?.flush()?),
                "status" => {
                    let p = s.persistence.as_mut().ok_or("missing recovery worker")?;
                    let errors = p
                        .drain_events()
                        .into_iter()
                        .filter_map(|e| match e {
                            WorkerEvent::Error(msg) => Some(msg),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    json!({
                        "failedAttempts": COUNTER.0.load(Ordering::Relaxed),
                        "ack": p.status.journaled_revision,
                        "session": p.session_dir,
                        "errors": errors,
                        "revision": s.revision,
                        "dirty": s.is_dirty()
                    })
                }
                "cut" => {
                    s.execute("sequence.open", json!({"item":reel}))?;
                    let seq = s.project.sequence(reel).ok_or("missing reel")?;
                    let duration = seq.video_tracks.iter().flat_map(|track| &track.items).find(|i| i.id == clip).ok_or("missing clip")?.duration;
                    let picture = s.try_render_program_at(0.125, sample)?.over_black_rgba8();
                    let lit = picture.as_chunks::<4>().0.iter().any(|[r, g, _, _]| *r > 100 && r > g);
                    json!({"ticks": duration, "lit": lit, "deferred": s.has_deferred_timelines()})
                }
                _ => return Err("unknown command".into()),
            })
        })();
        println!(
            "{}",
            match result {
                Ok(value) => json!({"ok":value}),
                Err(error) => json!({"error":error.to_string()}),
            }
        );
        io::stdout().flush()?;
    }
    Ok(())
}
