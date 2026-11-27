//! A read-only window for prepared fixtures. The partial session is private to this
//! viewer. Its only interactions are bin navigation and rendering a selected timeline.
#[path = "cache.rs"]
mod cache;
#[path = "discovery.rs"]
pub(super) mod discovery;
use super::{Result, reader::Reader};
use egui_kittest::{Harness, kittest::Queryable};
use filmcraft_engine::Session;
use filmcraft_format::{MAX_PROJECT_BYTES, atomic_write_new, project_folder::FolderDirectory};
use filmcraft_media::reader::read_regular_file_bounded;
use filmcraft_project::{BinEntry, BinId, ItemId, ProjectItem, ProjectView};
use filmcraft_ui_egui::i18n::t;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Instant};

#[derive(Serialize, Deserialize)]
struct Label {
    name: String,
    sequence: bool,
    // Older checksummed indexes are still readable. Refresh them once to classify every record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    class: Option<RecordClass>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum RecordClass {
    Graphic,
    Media,
    NonMedia,
}
impl From<&ProjectItem> for Label {
    fn from(item: &ProjectItem) -> Self {
        use filmcraft_project::ItemKind;
        let class = match &item.kind {
            ItemKind::Graphic { .. } => RecordClass::Graphic,
            ItemKind::Media(_) => RecordClass::Media,
            _ => RecordClass::NonMedia,
        };
        Self { name: item.name.clone(), sequence: item.as_sequence().is_some(), class: Some(class) }
    }
}

#[derive(Serialize, Deserialize)]
struct Index {
    manifest: String,
    labels: BTreeMap<ItemId, Label>,
    // The original preview index contained only the manifest and labels.
    #[serde(default)]
    keys: BTreeMap<ItemId, String>,
    #[serde(default)]
    checksum: Option<String>,
}

impl Index {
    fn digest(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&(&self.manifest, &self.keys, &self.labels))?)))
    }

    fn read(path: &Path) -> Result<Self> {
        let index: Self = serde_json::from_slice(&read_regular_file_bounded(path, MAX_PROJECT_BYTES)?)?;
        if let Some(checksum) = &index.checksum {
            if *checksum != index.digest()? || !index.keys.keys().eq(index.labels.keys()) {
                return Err("damaged name index; rebuild from the published project".into());
            }
        } else if !index.keys.is_empty() {
            return Err("incomplete name index".into());
        }
        Ok(index)
    }

    fn build(directory: &FolderDirectory, old: Option<Self>) -> Result<(Self, usize, u64)> {
        let mut old = old.filter(|index| index.checksum.is_some());
        let mut remaining = directory.remaining_item_bytes;
        let before = remaining;
        let mut labels = BTreeMap::new();
        let mut keys = BTreeMap::new();
        let mut decoded = 0;
        for (id, key) in directory.item_keys() {
            let reused = old
                .as_mut()
                .filter(|index| index.keys.get(&id).is_some_and(|k| k == key))
                .and_then(|index| index.labels.remove(&id))
                .filter(|label| label.class.is_some());
            let label = match reused {
                Some(label) => label,
                None => {
                    decoded += 1;
                    Label::from(&directory.read_item(id, &mut remaining)?)
                }
            };
            labels.insert(id, label);
            keys.insert(id, key.to_owned());
        }
        let mut next = Index { manifest: directory.revision.clone(), labels, keys, checksum: None };
        next.checksum = Some(next.digest()?);
        Ok((next, decoded, before - remaining))
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        let data = serde_json::to_vec(self)?;
        if data.len() as u64 > MAX_PROJECT_BYTES {
            return Err("name index exceeds the size limit".into());
        }
        Ok(data)
    }
}

pub(super) fn index(path: &Path, out: &Path) -> Result<()> {
    refresh(path, None, out)
}

pub(super) fn refresh(path: &Path, prior: Option<&Path>, out: &Path) -> Result<()> {
    let start = Instant::now();
    let old = prior.map(Index::read).transpose()?;
    let directory = Reader::open(path)?.directory;
    let (next, decoded, item_bytes) = Index::build(&directory, old)?;
    let data = next.bytes()?;
    atomic_write_new(out, &data)?;
    println!(
        "{}",
        json!({"items":next.labels.len(),"decoded":decoded,"itemBytes":item_bytes,"directoryBytes":directory.bytes_read,"indexBytes":data.len(),"ms":start.elapsed().as_secs_f64()*1000.0})
    );
    Ok(())
}

struct Preview {
    session: Session,
    reader: Option<Reader>,
    labels: BTreeMap<ItemId, Label>,
    view: ProjectView,
    bin: BinId,
    image: egui::ColorImage,
    texture: Option<egui::TextureHandle>,
    error: Option<String>,
}

impl Preview {
    fn open(path: &Path, index: Option<&Path>) -> Result<Self> {
        if let Some(index) = index {
            return Self::from_index(path, Reader::open(path)?, Index::read(index)?);
        }
        let mut session = Session::local_folder_editor();
        session.execute("file.open", json!({"path":path}))?;
        let labels = session.project.items.iter().map(|(&id, item)| (id, Label::from(item))).collect();
        let view = session.project_view();
        Self::from_session(session, None, labels, view)
    }

    fn from_index(path: &Path, mut reader: Reader, catalog: Index) -> Result<Self> {
        if reader.directory.revision != catalog.manifest || !reader.inventory().eq(catalog.labels.keys().copied()) {
            return Err("name index does not match this project manifest".into());
        }
        let mut session = Session::default();
        session.project = Arc::new(reader.take_project()?);
        session.path = Some(path.to_str().ok_or("invalid project path")?.to_owned());
        let view = reader.view.clone();
        Self::from_session(session, Some(reader), catalog.labels, view)
    }

    fn from_session(session: Session, reader: Option<Reader>, labels: BTreeMap<ItemId, Label>, view: ProjectView) -> Result<Self> {
        let bin = session.project.root.id;
        let active = view.active_sequence.ok_or("preview requires a saved active timeline")?;
        if !view.open_sequences.contains(&active) || view.open_sequences.iter().any(|id| !labels.get(id).is_some_and(|l| l.sequence)) {
            return Err("invalid saved timeline tabs".into());
        }
        let mut preview = Self { reader, session, labels, view, bin, image: egui::ColorImage::default(), texture: None, error: None };
        preview.select(active)?;
        Ok(preview)
    }

    fn select(&mut self, id: ItemId) -> Result<()> {
        if !self.labels.get(&id).is_some_and(|l| l.sequence) {
            return Err("select a sequence".into());
        }
        if let Some(reader) = &mut self.reader {
            let mut added = reader.missing(vec![id], &self.session.project.items)?;
            if added.iter().any(|(id, item)| self.labels.get(id).is_none_or(|label| label.name != item.name || label.sequence != item.as_sequence().is_some()))
            {
                return Err("record differs from the name index".into());
            }
            if !added.is_empty() {
                Arc::make_mut(&mut self.session.project).items.append(&mut added);
                self.session.media.add_stored(&self.session.project, self.session.path.as_deref());
            }
        }
        let seq = self.session.project.sequence(id).ok_or("timeline record is missing")?;
        let playhead = self.view.playheads.get(&id).copied().unwrap_or_default().clamp(filmcraft_time::Tick::ZERO, ProjectView::MAX_PLAYHEAD);
        let playhead = seq.settings.frame_rate.snap(playhead);
        let previous = self.session.state.active_sequence.replace(id);
        let frame = match self.session.try_render_program_at(0.25, playhead) {
            Ok(frame) => frame,
            Err(error) => {
                self.session.state.active_sequence = previous;
                return Err(error.into());
            }
        };
        self.session.set_playhead(playhead);
        if !self.view.open_sequences.contains(&id) {
            self.view.open_sequences.push(id);
        }
        self.view.active_sequence = Some(id);
        self.image = egui::ColorImage::from_rgba_unmultiplied([frame.w, frame.h], &frame.over_black_rgba8());
        self.texture = None;
        Ok(())
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let mut select = None;
        ui.horizontal_wrapped(|ui| {
            for id in &self.view.open_sequences {
                ui.push_id(("tab", id.0), |ui| {
                    let selected = Some(*id) == self.view.active_sequence;
                    let tab = ui.selectable_label(selected, &self.labels[id].name);
                    ui.ctx().accesskit_node_builder(tab.id, |node| {
                        node.set_role(egui::accesskit::Role::Tab);
                        node.set_selected(selected);
                    });
                    if tab.clicked() {
                        select = Some(*id);
                    }
                });
            }
        });
        let image = self.texture.get_or_insert_with(|| ui.ctx().load_texture("program", self.image.clone(), egui::TextureOptions::LINEAR));
        let size = image.size_vec2();
        let scale = (ui.available_width() / size.x).min(ui.available_height() * 0.5 / size.y);
        ui.add(egui::Image::from_texture(&*image).fit_to_exact_size(size * scale).alt_text(t("Program Monitor")));
        if let Some(error) = &self.error {
            let label = ui.label(error);
            ui.ctx().accesskit_node_builder(label.id, |n| n.set_live(egui::accesskit::Live::Polite));
        }
        if ui.button(t("Project root")).clicked() {
            self.bin = self.session.project.root.id;
        }
        let Some(bin) = self.session.project.root.find_bin(self.bin) else { return };
        let heading = ui.heading(&bin.name);
        ui.ctx().accesskit_node_builder(heading.id, |node| {
            node.set_role(egui::accesskit::Role::Heading);
            node.set_label(bin.name.clone());
            node.set_live(egui::accesskit::Live::Polite);
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
            for child in &bin.children {
                match child {
                    BinEntry::Bin(b) => {
                        if ui.push_id(("bin", b.id.0), |ui| ui.button(&b.name)).inner.clicked() {
                            self.bin = b.id;
                        }
                    }
                    BinEntry::Item(id) => {
                        let label = &self.labels[id];
                        if label.sequence {
                            if ui.push_id(("item", id.0), |ui| ui.button(&label.name)).inner.clicked() {
                                select = Some(*id);
                            }
                        } else {
                            ui.label(&label.name);
                        }
                    }
                }
            }
        });
        if let Some(id) = select {
            self.error = self.select(id).err().map(|e| e.to_string());
            ui.ctx().request_repaint();
        }
    }
}

impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

pub(super) fn window(path: &Path, index: Option<&Path>) -> Result<()> {
    eframe::run_native(
        t("FilmCraft · Read-only preview"),
        eframe::NativeOptions::default(),
        Box::new(move |_| {
            let preview = match index {
                Some(index) => Preview::open(path, Some(index)),
                None => cache::open(path),
            }
            .map_err(|error| std::io::Error::other(error.to_string()))?;
            Ok(Box::new(preview))
        }),
    )?;
    Ok(())
}

// A completed GPU readback also includes texture upload and painting. Initialize the renderer
// before timing so device creation isn't counted as project I/O.
pub(super) fn measure_frames(path: &Path, index: Option<&Path>, bin: BinId) -> Result<()> {
    let mut h = Harness::builder().with_size(egui::vec2(960.0, 720.0)).wgpu().build_ui_state(
        |ui, state: &mut Option<Preview>| {
            if let Some(view) = state {
                view.show(ui);
            }
        },
        None,
    );
    h.render()?;
    let start = Instant::now();
    *h.state_mut() = Some(Preview::open(path, index)?);
    h.step();
    h.render()?;
    let first = start.elapsed();
    h.query_by_label(t("Program Monitor")).ok_or("program image missing from rendered view")?;
    let preview = h.state().as_ref().ok_or("missing preview")?;
    let next = preview.view.open_sequences.iter().copied().find(|id| Some(*id) != preview.view.active_sequence).ok_or("expected a deferred timeline tab")?;
    let folder = preview.session.project.root.find_bin(bin).ok_or("unknown bin")?;
    let count = folder.children.len();
    let bin_name = folder.name.clone();
    let bytes = preview.reader.as_ref().map(|r| r.bytes);
    let resident = preview.session.project.items.len();
    let start = Instant::now();
    h.state_mut().as_mut().ok_or("missing preview")?.bin = bin;
    h.step();
    h.render()?;
    let listing = start.elapsed();
    h.query_by_role_and_label(egui::accesskit::Role::Heading, &bin_name).ok_or("requested bin missing from rendered view")?;
    let preview = h.state_mut().as_mut().ok_or("missing preview")?;
    if bytes != preview.reader.as_ref().map(|r| r.bytes) || resident != preview.session.project.items.len() {
        return Err("listing loaded project items".into());
    }
    let start = Instant::now();
    preview.select(next)?;
    h.step();
    h.render()?;
    let switched = start.elapsed();
    h.query_by_label(t("Program Monitor")).ok_or("program image missing from rendered view")?;
    let preview = h.state().as_ref().ok_or("missing preview")?;
    println!(
        "{}",
        json!({"firstFrameMs":first.as_secs_f64()*1000.0,"listingMs":listing.as_secs_f64()*1000.0,
        "switchFrameMs":switched.as_secs_f64()*1000.0,"listed":count,"openItems":resident,"switchItems":preview.session.project.items.len()-resident,
        "switchBytes":preview.reader.as_ref().zip(bytes).map(|(r,b)|r.bytes-b)})
    );
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;
