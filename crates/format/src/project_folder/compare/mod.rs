//! Read-only editorial comparison of two reachable saved revisions.
mod sequence;
mod snapshot;
use super::{bins::Entry, check_hash, invalid, recovery};
use serde_json::{Value, json};
use snapshot::Snapshot;
use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::Path,
};

#[derive(Clone, Copy)]
pub enum Selection {
    Bin(u64),
    Sequence(u64),
}
impl Selection {
    fn key(self) -> Entry {
        match self {
            Self::Bin(id) => Entry::Bin(id),
            Self::Sequence(id) => Entry::Item(id),
        }
    }
}

fn without(value: &Value, fields: &[&str]) -> Value {
    let mut copy = value.clone();
    if let Some(obj) = copy.as_object_mut() {
        for field in fields {
            obj.remove(*field);
        }
    }
    copy
}

fn same_except(left: &Value, right: &Value, fields: &[&str]) -> bool {
    let (Some(l), Some(r)) = (left.as_object(), right.as_object()) else { return left == right };
    let keep = |key: &String| !fields.contains(&key.as_str());
    l.iter().filter(|(key, _)| keep(key)).all(|(key, value)| r.get(key) == Some(value)) && r.keys().filter(|key| keep(key)).all(|key| l.contains_key(key))
}

fn changes(a: &Snapshot, b: &Snapshot, selection: Selection, rows: &mut Vec<Value>) {
    let keys: BTreeSet<_> = a.records.keys().chain(b.records.keys()).copied().collect();
    for key in keys {
        let (old, new) = (a.records.get(&key), b.records.get(&key));
        let (parent_a, parent_b) = (a.parents.get(&key), b.parents.get(&key));
        let mut kinds = Vec::new();
        match (parent_a, parent_b) {
            (Some(_), None) if old.is_some() && (b.hierarchy_complete || !b.catalog.contains(&key)) => kinds.push("removed"),
            (None, Some(_)) if new.is_some() && (a.hierarchy_complete || !a.catalog.contains(&key)) => kinds.push("added"),
            (Some(p), Some(q)) if p != q && !a.ambiguous.contains(&key) && !b.ambiguous.contains(&key) => kinds.push("moved"),
            _ => (),
        }
        if let (Some(left), Some(right)) = (old, new) {
            if left["name"] != right["name"] {
                kinds.push("renamed");
            }
            match key {
                Entry::Bin(id) => {
                    if !same_except(left, right, &["name", "children"]) {
                        kinds.push("other");
                    }
                    if let (Some(l), Some(r)) = (a.children.get(&id), b.children.get(&id)) {
                        let common = |x: &&Entry| {
                            a.parents.get(x) == Some(&Some(id)) && b.parents.get(x) == Some(&Some(id)) && !a.ambiguous.contains(x) && !b.ambiguous.contains(x)
                        };
                        let before: Vec<_> = l.iter().filter(common).collect();
                        let after: Vec<_> = r.iter().filter(common).collect();
                        if before != after {
                            rows.push(json!({"bin":id,"change":"reordered","before":before,"after":after}));
                        }
                    }
                }
                Entry::Item(id) => {
                    if !matches!(selection,Selection::Sequence(s) if s==id) && left != right && !same_except(left, right, &["name"]) {
                        kinds.push(if right.pointer("/kind/Sequence").is_some() { "sequenceChanged" } else { "other" });
                    }
                    if a.sidecars.get(&id) != b.sidecars.get(&id) {
                        kinds.push("associatedMetadata");
                    }
                }
            }
        }
        if !kinds.is_empty() {
            let mut row = json!({"record":key,"changes":kinds,"beforeName":old.map(|v| &v["name"]),"afterName":new.map(|v| &v["name"]),
                "beforeParent":parent_a,"afterParent":parent_b,"scope":if parent_a.is_none() && parent_b.is_none() {"dependency"}else{"selection"}});
            if old.into_iter().chain(new).any(|v| v.pointer("/kind/Sequence").is_some()) {
                row["itemType"] = json!("sequence");
            }
            if kinds.contains(&"removed") {
                row["absentFromProject"] = json!(!b.catalog.contains(&key));
            }
            rows.push(row);
        }
    }
    if let Selection::Sequence(id) = selection {
        let empty = sequence::Cut::default();
        if (a.cut.is_some() || !a.catalog.contains(&Entry::Item(id))) && (b.cut.is_some() || !b.catalog.contains(&Entry::Item(id))) {
            sequence::compare(id, a.cut.as_ref().unwrap_or(&empty), b.cut.as_ref().unwrap_or(&empty), rows);
        }
    }
}

/// Writes deterministic JSON lines. A partial report ends with complete=false and an error return.
pub fn compare(folder: &Path, from: &str, to: &str, selection: Selection, mut out: impl Write) -> io::Result<()> {
    check_hash(from)?;
    check_hash(to)?;
    let (mut left, mut right) = (None, None);
    // Both digests must be reachable from the same read of project.fcmod, even during publication.
    recovery::walk(folder, |hash, manifest| {
        if hash == from {
            left = Some(Snapshot::read(folder, manifest, selection).map_err(|e| invalid(format!("{hash}: {e}")))?);
        }
        if hash == to && to != from {
            right = Some(Snapshot::read(folder, manifest, selection).map_err(|e| invalid(format!("{hash}: {e}")))?);
        }
        Ok(left.is_some() && (from == to || right.is_some()))
    })?;
    let absent = |hash: &str| invalid(format!("revision {hash} is outside the current project history"));
    let a = left.as_ref().ok_or_else(|| absent(from))?;
    let b = if from == to { a } else { right.as_ref().ok_or_else(|| absent(to))? };
    let key = selection.key();
    if !a.catalog.contains(&key) && !b.catalog.contains(&key) {
        return Err(invalid(format!("missing {key:?} in both revisions")));
    }
    let mut rows = Vec::new();
    let complete = a.errors.is_empty() && b.errors.is_empty();
    changes(a, b, selection, &mut rows);
    for (hash, side) in [(from, a), (to, b)] {
        for (key, reason) in &side.errors {
            rows.push(json!({"revision":hash,"record":key,"error":reason}));
        }
        if from == to {
            break;
        }
    }
    rows.push(json!({"complete":complete,"from":from,"to":to,"selection":key}));
    for row in rows {
        serde_json::to_writer(&mut out, &row)?;
        writeln!(out)?;
    }
    if complete { Ok(()) } else { Err(invalid("incomplete revision comparison")) }
}
