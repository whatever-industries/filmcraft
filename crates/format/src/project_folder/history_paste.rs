//! Prepare an edit in memory. The session owns Undo and explicit publication.
use super::{ProjectRecords, ROOT, digest, encode_at, invalid, recovery};
use filmcraft_project::{Project, ProjectItem};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

const ORIGIN: &str = "modular_history_origin";

pub(super) fn allocate(next: &mut u64) -> io::Result<u64> {
    let id = *next;
    if id >= Project::MAX_NEXT_ID {
        return Err(invalid("project ID space exhausted"));
    }
    *next += 1;
    Ok(id)
}

fn elements(v: &mut Value) -> impl Iterator<Item = &mut Value> {
    v.as_array_mut().into_iter().flatten()
}

// Walk only schema-owned links to preserve unknown fields, including numeric extensions.
fn remap_links(raw: &mut Value, items: &BTreeMap<u64, u64>) -> io::Result<()> {
    let change = |v: &mut Value| -> io::Result<()> {
        if let Some(id) = v.as_u64() {
            *v = (*items.get(&id).ok_or_else(|| invalid(format!("unresolved source {id}")))?).into();
        }
        Ok(())
    };
    if let Some(v) = raw.pointer_mut("/kind/Subclip/parent") {
        change(v)?;
    }
    if let Some(seq) = raw.pointer_mut("/kind/Sequence") {
        for track_kind in ["video_tracks", "audio_tracks", "submix_tracks"] {
            for track in seq.get_mut(track_kind).into_iter().flat_map(elements) {
                for clip in elements(&mut track["items"]) {
                    change(&mut clip["item"])?;
                }
            }
        }
        for camera in seq.pointer_mut("/multicam/cameras").into_iter().flat_map(elements) {
            if let Some(v) = camera.get_mut("source") {
                change(v)?;
            }
        }
        if let Some(merged) = seq.get_mut("merged").filter(|v| v.is_object()) {
            if let Some(v) = merged.get_mut("video") {
                change(v)?;
            }
            for v in merged.get_mut("audio").into_iter().flat_map(elements) {
                change(v)?;
            }
        }
    }
    Ok(())
}

fn fresh_local_ids(raw: &mut Value, next: &mut u64) -> io::Result<()> {
    let mut ids = BTreeMap::new();
    let mut fresh = |v: &mut Value| -> io::Result<()> {
        if let Some(old) = v.as_u64() {
            let new = match ids.entry(old) {
                std::collections::btree_map::Entry::Occupied(e) => *e.get(),
                std::collections::btree_map::Entry::Vacant(e) => *e.insert(allocate(next)?),
            };
            *v = new.into();
        }
        Ok(())
    };
    fn fields(v: &mut Value, keys: &[&str], assign: &mut impl FnMut(&mut Value) -> io::Result<()>) -> io::Result<()> {
        for key in keys {
            if let Some(v) = v.get_mut(*key) {
                assign(v)?;
            }
        }
        Ok(())
    }
    fn mixer(v: &mut Value, assign: &mut impl FnMut(&mut Value) -> io::Result<()>) -> io::Result<()> {
        fields(v, &["output"], assign)?;
        for send in v.get_mut("sends").into_iter().flat_map(elements) {
            fields(send, &["target"], assign)?;
        }
        Ok(())
    }
    for marker in raw.pointer_mut("/kind/Media/markers").into_iter().flat_map(elements) {
        fields(marker, &["id"], &mut fresh)?;
    }
    if let Some(seq) = raw.pointer_mut("/kind/Sequence") {
        for marker in seq.get_mut("markers").into_iter().flat_map(elements) {
            fields(marker, &["id"], &mut fresh)?;
        }
        if let Some(master) = seq.get_mut("master_mixer") {
            mixer(master, &mut fresh)?;
        }
        for kind in ["video_tracks", "audio_tracks", "submix_tracks", "caption_tracks"] {
            for track in seq.get_mut(kind).into_iter().flat_map(elements) {
                fields(track, &["id"], &mut fresh)?;
                if let Some(strip) = track.get_mut("mixer") {
                    mixer(strip, &mut fresh)?;
                }
                for clip in track.get_mut("items").into_iter().flat_map(elements) {
                    fields(clip, &["id", "link", "group"], &mut fresh)?;
                    for marker in clip.get_mut("markers").into_iter().flat_map(elements) {
                        fields(marker, &["id"], &mut fresh)?;
                    }
                }
                for transition in track.get_mut("transitions").into_iter().flat_map(elements) {
                    fields(transition, &["id", "from", "to"], &mut fresh)?;
                }
                for caption in track.get_mut("captions").into_iter().flat_map(elements) {
                    fields(caption, &["id"], &mut fresh)?;
                }
            }
        }
        for camera in seq.pointer_mut("/multicam/cameras").into_iter().flat_map(elements) {
            fields(camera, &["video_track"], &mut fresh)?;
            for t in camera.get_mut("audio_tracks").into_iter().flat_map(elements) {
                fresh(t)?;
            }
        }
    }
    Ok(())
}

fn sidecars(records: &ProjectRecords, id: u64) -> Value {
    let mut side = serde_json::Map::new();
    for kind in recovery::SIDECARS {
        if let Some(v) = records.envelope["project"][kind].get(id.to_string()) {
            side.insert(kind.into(), v.clone());
        }
    }
    Value::Object(side)
}

fn fingerprint(raw: &Value, side: &Value) -> io::Result<String> {
    let mut raw = raw.clone();
    let obj = raw.as_object_mut().ok_or_else(|| invalid("expected item"))?;
    for key in [ORIGIN, "field_edits"] {
        obj.remove(key);
    }
    Ok(digest(&serde_json::to_vec(&(&raw, side))?))
}

// Iterative postorder handles deep nested cuts and rejects cycles without recursion.
fn dependency_order(records: &ProjectRecords, selected: &[u64]) -> io::Result<Vec<u64>> {
    let mut result = Vec::new();
    let mut pending: Vec<_> = selected.iter().rev().map(|&id| (id, false)).collect();
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    while let Some((id, returning)) = pending.pop() {
        if done.contains(&id) {
            continue;
        }
        if returning {
            visiting.remove(&id);
            done.insert(id);
            result.push(id);
            continue;
        }
        if !visiting.insert(id) {
            return Err(invalid(format!("cyclic dependency at item {id}")));
        }
        let raw = records.items.get(&id).ok_or_else(|| invalid(format!("missing item {id}")))?;
        let item: ProjectItem = serde::Deserialize::deserialize(raw)?;
        pending.push((id, true));
        item.visit_references(|link| pending.push((link.0, false)));
    }
    Ok(result)
}

pub struct Paste {
    pub document: Vec<u8>,
    pub selected: Vec<u64>,
    pub next_id: u64,
}

pub fn prepare(folder: &Path, revision: &str, selected: &[u64], bin: u64, current: &[u8], mut next: u64) -> io::Result<Paste> {
    let selected_set: BTreeSet<_> = selected.iter().copied().collect();
    if selected_set.is_empty() || selected_set.len() != selected.len() || selected.len() > super::RECORD_LIMIT {
        return Err(invalid("select distinct historical items"));
    }
    let manifest = recovery::at(folder, revision)?;
    let mut budget = crate::MAX_PROJECT_BYTES;
    let (env, _) = recovery::metadata(folder, &manifest, &mut budget)?;
    let historical = recovery::extract_items(folder, &manifest, env, &mut budget, selected.to_vec())?;
    let mut live = ProjectRecords::split(current).map_err(invalid)?;
    if !live.bins.contains_key(&bin) {
        return Err(invalid(format!("missing destination bin {bin}")));
    }
    let mut copies = BTreeMap::<u64, Vec<u64>>::new();
    for (&id, item) in &live.items {
        let origin = &item[ORIGIN];
        if origin["revision"] == revision
            && let Some(old) = origin["item"].as_u64()
        {
            copies.entry(old).or_default().push(id);
        }
    }
    let mut mapped = BTreeMap::new();
    let mut created = BTreeSet::new();
    for old in dependency_order(&historical, selected)? {
        let mut raw = historical.items[&old].clone();
        remap_links(&mut raw, &mapped)?;
        let mut side = sidecars(&historical, old);
        for record in [&mut raw, &mut side] {
            super::history_luts::restore(record, &historical, &mut live, &mut next)?;
        }
        let signature = fingerprint(&raw, &side)?;
        // Only media and subclips support an additional ordinary filing.
        let share_filing = raw.pointer("/kind/Media").is_some() || raw.pointer("/kind/Subclip").is_some();
        let force_new = selected_set.contains(&old) && !share_filing;
        if !force_new {
            let mut reuse = None;
            for &id in copies.get(&old).into_iter().flatten() {
                let candidate = &live.items[&id];
                let origin = &candidate[ORIGIN];
                if origin["source"] == signature && origin["copy"] == fingerprint(candidate, &sidecars(&live, id))? {
                    reuse = Some(id);
                    break;
                }
            }
            if let Some(id) = reuse {
                mapped.insert(old, id);
                continue;
            }
            if let Some(existing) = live.items.get(&old)
                && fingerprint(existing, &sidecars(&live, old))? == signature
            {
                mapped.insert(old, old);
                continue;
            }
        }
        let id = allocate(&mut next)?;
        if live.items.contains_key(&id) {
            return Err(invalid(format!("allocated item {id} already exists")));
        }
        fresh_local_ids(&mut raw, &mut next)?;
        raw["id"] = id.into();
        raw["created"] = id.into();
        if let Some(obj) = raw.as_object_mut() {
            obj.remove("field_edits");
        }
        let copy = fingerprint(&raw, &side)?;
        raw[ORIGIN] = json!({"revision":revision,"item":old,"source":signature,"copy":copy});
        for kind in recovery::SIDECARS {
            if let Some(v) = side.get(kind) {
                live.envelope["project"][kind][id.to_string()] = v.clone();
            }
        }
        live.items.insert(id, raw);
        created.insert(id);
        mapped.insert(old, id);
    }
    let inventory: Vec<_> = live.items.keys().copied().collect();
    live.envelope["project"]["items"] = json!(inventory);
    live.envelope["project"]["next_id"] = next.into();
    let destination = live.bins.get_mut(&bin).ok_or_else(|| invalid("destination bin disappeared"))?;
    let ids: Vec<_> = selected.iter().map(|old| mapped[old]).collect();
    for &id in &ids {
        let entry = json!({"Item":id});
        let children = destination["children"].as_array_mut().ok_or_else(|| invalid("expected bin children"))?;
        if created.contains(&id) {
            children.push(entry);
        } else if !children.contains(&entry) {
            let bin = destination.as_object_mut().ok_or_else(|| invalid("invalid destination bin"))?;
            let links = bin.entry("filed_sources").or_insert_with(|| json!([])).as_array_mut().ok_or_else(|| invalid("invalid source filings"))?;
            if !links.contains(&json!(id)) {
                links.push(id.into());
            }
        }
    }
    Ok(Paste { document: encode_at(folder, live, &folder.join(ROOT))?, selected: ids, next_id: next })
}
