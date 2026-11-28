//! Copy the captured modular revision into local recovery storage one verified record at a time.
use super::{CheckpointStore, CheckpointWrite, compatible_document, digest, invalid, join};
use crate::MAX_PROJECT_BYTES;
use crate::project_folder::{FolderDirectory, overlay, revisions, verified_bytes};
use craft_fcproj_format::records::ProjectRecords;
use filmcraft_project::{ItemId, MediaRef, Project, ProjectItem};
use std::{io, path::Path, time::Instant};

/// Run the ordinary typed load checks with just this record resident. Repairs
/// involving other items are applied by the recovery reader to the whole draft.
fn checked_item(item: ProjectItem) -> io::Result<ProjectItem> {
    let id = item.id;
    let mut project = Project::new("");
    // This temporary model has no allocator state of its own. Only ID bounds matter here.
    project.next_id = Project::MAX_NEXT_ID;
    project.items.insert(id, item);
    crate::validate_records(&mut project).map_err(invalid)?;
    project.items.remove(&id).ok_or_else(|| invalid("missing checkpoint item"))
}

fn edited_bytes(directory: &FolderDirectory, original: &[u8], saved: &ProjectItem, working: &ProjectItem) -> io::Result<Vec<u8>> {
    let before = checked_item(directory.decode_item(saved.id, original)?)?;
    if before != *saved || working.id != saved.id {
        return Err(invalid("item differs from saved before-image"));
    }
    let edited = overlay::record(&serde_json::from_slice(original)?, &serde_json::to_value(saved)?, &serde_json::to_value(working)?, "edited item")?;
    let bytes = serde_json::to_vec(&edited)?;
    if checked_item(directory.decode_item(saved.id, &bytes)?)? != checked_item(working.clone())? {
        return Err(invalid("checkpoint differs from reviewed edit"));
    }
    Ok(bytes)
}

enum PartialEdit<'a> {
    Item(&'a ProjectItem, &'a ProjectItem, Option<u64>),
    Directory(&'a Project),
}

impl CheckpointStore {
    pub fn publish_directory(
        &mut self,
        session: &Path,
        path: &Path,
        expected: &str,
        project: &Project,
        marker: impl FnOnce(&str, usize, &str) -> io::Result<usize>,
    ) -> io::Result<CheckpointWrite> {
        project.validate_directory().map_err(invalid)?;
        self.publish_partial(session, path, expected, PartialEdit::Directory(project), marker)
    }
    pub fn publish_relink(
        &mut self,
        session: &Path,
        path: &Path,
        expected: &str,
        saved: &ProjectItem,
        working: &ProjectItem,
        publish_marker: impl FnOnce(&str, usize, &str) -> io::Result<usize>,
    ) -> io::Result<CheckpointWrite> {
        let identity = |item: &ProjectItem| item.as_media().filter(|m| matches!(m.media, MediaRef::File { .. })).and_then(|m| m.identity);
        if identity(saved).is_none() || identity(saved) != identity(working) {
            return Err(invalid("source differs from reviewed relink"));
        }
        self.publish_partial(session, path, expected, PartialEdit::Item(saved, working, None), publish_marker)
    }

    pub fn publish_trim(
        &mut self,
        session: &Path,
        path: &Path,
        expected: &str,
        items: (&ProjectItem, &ProjectItem),
        next_id: Option<u64>,
        publish_marker: impl FnOnce(&str, usize, &str) -> io::Result<usize>,
    ) -> io::Result<CheckpointWrite> {
        let (saved, working) = items;
        if saved.as_sequence().is_none() || working.as_sequence().is_none() {
            return Err(invalid("expected a sequence checkpoint"));
        }
        self.publish_partial(session, path, expected, PartialEdit::Item(saved, working, next_id), publish_marker)
    }

    fn publish_partial(
        &mut self,
        session: &Path,
        path: &Path,
        expected: &str,
        edit: PartialEdit,
        publish_marker: impl FnOnce(&str, usize, &str) -> io::Result<usize>,
    ) -> io::Result<CheckpointWrite> {
        let start = Instant::now();
        let folder = path.parent().ok_or_else(|| invalid("missing project directory"))?;
        let (_, mut records) = revisions::read_base(folder, expected)?;
        let mut directory = FolderDirectory::open(path)?;
        if directory.revision != expected {
            return Err(invalid("stale relink checkpoint base"));
        }
        let mut envelope = directory.envelope.take();
        match edit {
            PartialEdit::Item(_, working, next_id) => {
                crate::project_folder::relinked_source::set_trim_counter(&mut envelope, next_id, Some(working))?;
            }
            PartialEdit::Directory(project) => crate::project_folder::relinked_source::apply_directory(&mut envelope, project)?,
        }
        if let Some(fields) = envelope.as_object_mut() {
            fields.remove("view");
        }
        let header = serde_json::to_vec(&envelope)?;
        records.envelope = digest(&header);
        let mut header = Some(header);
        // Validate the envelope and bin graph with the recovery reader's join.
        // An item's ID suffices for resolving bin references and the inventory.
        // The actual payload and effective copy identity are checked when streamed.
        let items = directory.item_keys().map(|(id, _)| (id.0, serde_json::json!({"id": id}))).collect();
        let mut document = join(ProjectRecords { envelope, bins: std::mem::take(&mut directory.bins), items })?;
        document["project"]["items"] = serde_json::json!({});
        let name = compatible_document(document)?.project.name;
        let mut remaining = directory.remaining_item_bytes;
        let mut bin_budget = MAX_PROJECT_BYTES;
        records.layout = 1;
        records.revision = None;

        let mut replacement = if let PartialEdit::Item(saved, working, _) = edit {
            let (pos, (_, hash)) = records.items.iter_mut().enumerate().find(|(_, (id, _))| *id == saved.id.0).ok_or_else(|| invalid("missing edited item"))?;
            let bytes = edited_bytes(&directory, &verified_bytes(folder, hash, &mut remaining)?, saved, working)?;
            *hash = digest(&bytes);
            Some((pos, bytes))
        } else {
            None
        };
        let first_item = 1 + records.bins.len();
        let ids: Vec<_> = records.items.iter().map(|(id, _)| ItemId(*id)).collect();
        self.publish_records(
            session,
            records,
            start.elapsed(),
            None,
            |pos, hash, needed| {
                if pos == 0 {
                    return Ok(header.take());
                }
                if replacement.as_ref().is_some_and(|(index, _)| pos == first_item + index) {
                    return Ok(replacement.take().map(|(_, data)| data));
                }
                let budget = if pos < first_item { &mut bin_budget } else { &mut remaining };
                let bytes = verified_bytes(folder, hash, budget)?;
                if pos >= first_item {
                    checked_item(directory.decode_item(ids[pos - first_item], &bytes)?)?;
                }
                Ok(needed.then_some(bytes))
            },
            |root, bytes| {
                if FolderDirectory::published_revision(path)? != expected {
                    return Err(invalid("stale relink checkpoint base"));
                }
                publish_marker(root, bytes, &name)
            },
        )
    }
}
