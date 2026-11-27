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

fn visit_owners(root: &Bin, mut visit: impl FnMut(ItemId, BinId)) {
    let mut todo = vec![root];
    while let Some(bin) = todo.pop() {
        for entry in &bin.children {
            match entry {
                BinEntry::Bin(child) => todo.push(child),
                BinEntry::Item(item) => visit(*item, bin.id),
            }
        }
    }
}

fn owners(root: &Bin) -> BTreeMap<ItemId, Option<BinId>> {
    let mut out = BTreeMap::new();
    visit_owners(root, |item, bin| {
        out.entry(item)
            .and_modify(|owner| {
                if *owner != Some(bin) {
                    *owner = None;
                }
            })
            .or_insert(Some(bin));
    });
    out
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn owning_bin(project: &Project, item: ItemId) -> Option<BinId> {
    owners(&project.root).get(&item).copied().flatten()
}

/// Enforce the stored-path policy and the source's reviewed fingerprint before
/// installing a recovered project, while playback and still probing cannot see it.
pub(crate) fn check_recovered_source(project: &mut Project, id: ItemId, document: Option<&str>, services: &dyn crate::Services) -> bool {
    let Some(media) = project.item_mut(id).and_then(|item| item.as_media_mut()) else { return false };
    let MediaRef::File { path } = &media.media else { return false };
    if media.offline {
        return true;
    }
    let actual = crate::doc_path::resolve(path, document.map(std::path::Path::new)).ok().and_then(|file| super::identity_of(services, &file).ok());
    if actual.is_some() && actual == media.identity {
        return false;
    }
    media.offline = true;
    true
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
    let rev = s.folder_revision().ok_or_else(|| fail("open the local project folder first"))?.to_owned();
    let owner = owning_bin(&s.project, item).ok_or_else(|| fail("source needs one owning bin"))?;
    let current = review_for(&s.project, &rev, owner, item, path).ok_or_else(|| fail("source has no saved file fingerprint"))?;
    if let Some(folder) = &s.editing_folder {
        if owning_bin(&folder.project, item) != Some(owner) {
            return Err(fail("source moved since discovery; save and review again"));
        }
        if Some(&current) != review_for(&folder.project, &rev, owner, item, path).as_ref() {
            return Err(fail("source changed since its last save; review again"));
        }
    }
    if p["reviewOnly"] == true {
        if p["revision"].as_str() != Some(rev.as_str()) {
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
    if p["reviewOnly"] == true {
        verified_candidate(s, &current)?;
        let manifest = s.path.as_deref().ok_or_else(|| fail("missing project folder"))?;
        if filmcraft_format::project_folder::FolderDirectory::published_revision(std::path::Path::new(manifest)).map_err(|e| fail(&e.to_string()))? != rev {
            return Err(fail("project changed; refresh and review again"));
        }
        return Ok(json!({"review":current}));
    }
    let _guard = if let Some(w) = &s.warm_folder {
        w.lock()?
    } else {
        s.editing_folder
            .as_ref()
            .ok_or_else(|| fail("missing project folder"))?
            .snapshot
            .lock_current_revision()
            .map_err(|e| fail(&format!("refresh and review again: {e}")))?
    };
    let saved = if s.warm_folder.is_some() {
        s.load_warm_impact(item)?;
        if !super::refresh(s).iter().any(|m| m.item == item.0 && m.status == "missing") {
            return Err(fail("the original is no longer missing; review again"));
        }
        Some(s.project.item(item).ok_or_else(|| fail("missing source"))?.clone())
    } else {
        None
    };
    let cand = verified_candidate(s, &current)?;
    let only_source = p.get("review").map(|_| item);
    let rows = apply(s, vec![Plan { item, cand }], false, None, only_source, "Link Media")?;
    if let (Some(w), Some(saved)) = (&mut s.warm_folder, saved) {
        w.source = Some((owner, saved));
        s.reviewed_relink = Some(item);
    }
    Ok(json!({"relinked": rows,"bin":owner}))
}

#[cfg(not(target_arch = "wasm32"))]
fn verified_candidate(s: &Session, current: &Review) -> Result<super::Candidate> {
    let cand = check_candidate(s, current.item, &current.path, &MatchOptions::default())?;
    if !cand.ok || cand.identity_match != Some(true) || cand.identity != Some(current.identity) {
        return Err(bad("media.relink", "candidate no longer passes the media and fingerprint checks; review again"));
    }
    Ok(cand)
}

/// Predict one identity-verified relink on an isolated model. The caller must supply the
/// complete affected record set and recheck the published revision before using the result.
#[cfg(not(target_arch = "wasm32"))]
pub fn impact(s: &Session, revision: &str, review: &Review) -> Result<Value> {
    predict(s, revision, review).map(|(report, _)| report)
}

/// For the local developer harness. The caller holds the saved revision lock and
/// has hydrated the complete impact set. Repeat prediction on the actual working model.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_source_only(s: &mut Session, revision: &str, review: &Review) -> Result<()> {
    let (report, plan) = predict(s, revision, review)?;
    if report["status"] != "source-only" {
        return Err(bad("relink-apply", format!("full editor required: {report}")));
    }
    if super::identity_of(&*s.services, &review.path).ok() != Some(review.identity) {
        return Err(bad("relink-apply", "candidate changed; refresh discovery"));
    }
    apply(s, vec![plan], false, None, Some(review.item), "Link Media")?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn predict(s: &Session, revision: &str, review: &Review) -> Result<(Value, Plan)> {
    let item = review.item;
    let owner = owning_bin(&s.project, item);
    if owner != Some(review.bin) || review_for(&s.project, revision, review.bin, item, &review.path).as_ref() != Some(review) {
        return Err(bad("relink-impact", "source or saved revision changed; refresh discovery"));
    }
    let cand = verified_candidate(s, review)?;
    let users = super::script_users(s, item);
    let scripts: Vec<_> = super::scripts_needing_review(&s.project, &users, cand.identity).collect();
    let plan = Plan { item, cand };
    let mut changed = Vec::new();
    let status = if scripts.is_empty() {
        let mut model = (*s.project).clone();
        super::update_records(&mut model, &plan, &users, false);
        changed.extend(model.items.iter().filter(|(id, value)| s.project.item(**id) != Some(*value)).map(|(id, _)| *id));
        if changed.is_empty() {
            "unchanged"
        } else if changed == [item] {
            "source-only"
        } else {
            "full-editor"
        }
    } else {
        "script-review"
    };
    // Include every owning bin, with an empty array denoting an unfiled record.
    let wanted = changed.iter().chain(&scripts);
    let mut bins: BTreeMap<_, std::collections::BTreeSet<BinId>> = wanted.map(|id| (*id, Default::default())).collect();
    visit_owners(&s.project.root, |id, bin| {
        if let Some(owners) = bins.get_mut(&id) {
            owners.insert(bin);
        }
    });
    let unfiled: Vec<_> = changed.iter().filter(|id| bins.get(id).is_some_and(|bins| bins.is_empty())).collect();
    Ok((json!({"revision":revision, "source":item, "status":status, "changed":changed,"scriptReview":scripts,"owners":bins,"unfiled":unfiled}), plan))
}
