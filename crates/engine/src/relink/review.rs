//! A discovery result authorizes no writes. Recheck it in the full local editor at Apply.
#[cfg(not(target_arch = "wasm32"))]
use super::{MatchOptions, Plan, Result, Session, apply, bad, check_candidate};
use filmcraft_project::{Bin, BinEntry, BinId, ItemId, MediaIdentity, MediaRef, Project};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub revision: String,
    pub bin: BinId,
    pub item: ItemId,
    pub path: String,
    pub identity: MediaIdentity,
    pub source_path: String,
}

fn owners(root: &Bin) -> BTreeMap<ItemId, Option<BinId>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root];
    while let Some(bin) = todo.pop() {
        for entry in &bin.children {
            match entry {
                BinEntry::Bin(child) => todo.push(child),
                BinEntry::Item(item) => {
                    out.entry(*item)
                        .and_modify(|owner| {
                            if *owner != Some(bin.id) {
                                *owner = None;
                            }
                        })
                        .or_insert(Some(bin.id));
                }
            }
        }
    }
    out
}

fn review_for(project: &Project, revision: &str, bin: BinId, item: ItemId, path: &str) -> Option<Review> {
    let media = project.item(item)?.as_media()?;
    let MediaRef::File { path: original } = &media.media else { return None };
    Some(Review { revision: revision.into(), bin, item, path: path.into(), identity: media.identity?, source_path: original.clone() })
}

/// Add navigable bin IDs to results and a review to candidates positively matched at discovery.
pub fn annotate(project: &Project, revision: &str, report: &mut Value) {
    report["revision"] = json!(revision);
    let owners = owners(&project.root);
    for row in report["results"].as_array_mut().into_iter().flatten() {
        let Some(item) = row["item"].as_u64().map(ItemId) else { continue };
        let bin = owners.get(&item).copied().flatten();
        row["bin"] = json!(bin);
        let Some(bin) = bin else { continue };
        for candidate in row["candidates"].as_array_mut().into_iter().flatten() {
            if candidate["ok"] == true
                && candidate["identityMatch"] == true
                && let Some(path) = candidate["path"].as_str()
            {
                candidate["review"] = json!(review_for(project, revision, bin, item, path));
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn verified(s: &mut Session, p: &Value, item: ItemId, path: &str) -> Result<Value> {
    let fail = |msg: &str| bad("media.relink", msg);
    let folder = s.editing_folder.as_ref().ok_or_else(|| fail("open the local project folder first"))?;
    let rev = folder.snapshot.revision();
    let owner = owners(&s.project.root).get(&item).copied().flatten().ok_or_else(|| fail("source needs one owning bin"))?;
    if owners(&folder.project.root).get(&item) != Some(&Some(owner)) {
        return Err(fail("source moved since discovery; save and review again"));
    }
    let current = review_for(&s.project, rev, owner, item, path).ok_or_else(|| fail("source has no saved file fingerprint"))?;
    if Some(&current) != review_for(&folder.project, rev, owner, item, path).as_ref() {
        return Err(fail("source changed since its last save; review again"));
    }
    if p["reviewOnly"] == true {
        if p["revision"].as_str() != Some(rev) {
            return Err(fail("project changed; refresh and review again"));
        }
    } else {
        let reviewed: Review = serde_json::from_value(p.get("review").unwrap_or(&p["fullEditorReview"]).clone())
            .map_err(|_| fail("expected a reviewed source, bin, revision and fingerprint"))?;
        if reviewed != current {
            return Err(fail("project, source or candidate changed; refresh and review again"));
        }
        if s.state.project_selection.as_slice() != [item] || p["bin"] != json!(owner) {
            return Err(fail("select the source in its owning bin"));
        }
    }
    let cand = check_candidate(s, item, path, &MatchOptions::default())?;
    if !cand.ok || cand.identity_match != Some(true) || cand.identity != Some(current.identity) {
        return Err(fail("candidate no longer passes the media and fingerprint checks; review again"));
    }
    if p["reviewOnly"] == true {
        let manifest = folder.snapshot.directory().map_err(|e| fail(&e.to_string()))?.join(filmcraft_format::project_folder::ROOT);
        if filmcraft_format::project_folder::FolderDirectory::published_revision(&manifest).map_err(|e| fail(&e.to_string()))? != rev {
            return Err(fail("project changed; refresh and review again"));
        }
        return Ok(json!({"review":current}));
    }
    let _guard = folder.snapshot.lock_current_revision().map_err(|e| fail(&format!("refresh and review again: {e}")))?;
    // Only an explicit handoff to the full editor can accept dependent record edits.
    let only_source = p.get("review").map(|_| item);
    let rows = apply(s, vec![Plan { item, cand }], false, None, only_source, "Link Media")?;
    Ok(json!({"relinked": rows,"bin":owner}))
}
