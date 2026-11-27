//! Selective saves combine one bin from one project with the rest of another.
//! Bin children are independent records even though Project represents them as a tree.
use crate::{EngineError, Result};
use filmcraft_project::{Bin, BinEntry, BinId, ItemId, Project};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Member {
    Item(ItemId),
    Bin(BinId),
}

fn members(bin: &Bin) -> impl Iterator<Item = Member> + '_ {
    bin.children.iter().map(|c| match c {
        BinEntry::Item(i) => Member::Item(*i),
        BinEntry::Bin(b) => Member::Bin(b.id),
    })
}

#[derive(Default)]
struct Index<'a> {
    bins: BTreeMap<BinId, (&'a Bin, Option<BinId>)>,
    owners: BTreeMap<ItemId, BTreeSet<BinId>>,
}

impl<'a> Index<'a> {
    fn new(root: &'a Bin) -> Self {
        let mut index = Self::default();
        let mut pending = vec![(root, None)];
        while let Some((bin, parent)) = pending.pop() {
            index.bins.insert(bin.id, (bin, parent));
            for entry in &bin.children {
                match entry {
                    BinEntry::Bin(b) => pending.push((b, Some(bin.id))),
                    BinEntry::Item(i) => {
                        index.owners.entry(*i).or_default().insert(bin.id);
                    }
                }
            }
        }
        index
    }
}

pub(super) struct Status {
    pub bins: BTreeSet<BinId>,
    pub settings: bool,
    pub unfiled: bool,
}

impl Status {
    pub fn dirty(&self) -> bool {
        self.settings || self.unfiled || !self.bins.is_empty()
    }
}

pub(super) fn status(before: &Project, after: &Project) -> Status {
    let old = Index::new(&before.root);
    let new = Index::new(&after.root);
    let mut bins = BTreeSet::new();
    for (&id, &(bin, parent)) in &old.bins {
        if !new.bins.get(&id).is_some_and(|(b, p)| *p == parent && b.name == bin.name && members(bin).eq(members(b))) {
            bins.insert(id);
        }
    }
    bins.extend(new.bins.keys().filter(|id| !old.bins.contains_key(id)));
    let mut unfiled = false;
    // Deleting a project item can leave attached metadata behind. It still
    // belongs to the document even if the item is absent on both sides.
    let keys = |p: &'_ Project| -> BTreeSet<ItemId> {
        p.items.keys().chain(p.transcripts.keys()).chain(p.narrations.keys()).chain(p.source_graphics.keys()).copied().collect()
    };
    for id in keys(before).union(&keys(after)) {
        if before.items.get(id) != after.items.get(id)
            || before.transcripts.get(id) != after.transcripts.get(id)
            || before.narrations.get(id) != after.narrations.get(id)
            || before.source_graphics.get(id) != after.source_graphics.get(id)
        {
            let owners = [old.owners.get(id), new.owners.get(id)];
            unfiled |= owners == [None, None];
            bins.extend(owners.into_iter().flatten().flatten());
        }
    }
    let settings = before.name != after.name
        || before.notes != after.notes
        || before.settings != after.settings
        || before.luts != after.luts
        || before.custom_colors != after.custom_colors
        || before.custom_color_names != after.custom_color_names
        || before.search_bins != after.search_bins;
    Status { bins, settings, unfiled }
}

fn conflict(reason: &str) -> EngineError {
    EngineError::Other(format!("cannot apply this bin independently: {reason}"))
}

fn direct_items(bin: Option<&Bin>) -> impl Iterator<Item = ItemId> + '_ {
    bin.into_iter().flat_map(members).filter_map(|m| if let Member::Item(id) = m { Some(id) } else { None })
}

fn copy_item(project: &mut Project, from: &Project, source: ItemId, dest: ItemId) {
    fn copy_entry<T: Clone>(target: &mut BTreeMap<ItemId, T>, from: &BTreeMap<ItemId, T>, source: ItemId, dest: ItemId) {
        if let Some(value) = from.get(&source) {
            target.insert(dest, value.clone());
        } else {
            target.remove(&dest);
        }
    }
    copy_entry(&mut project.items, &from.items, source, dest);
    copy_entry(&mut project.transcripts, &from.transcripts, source, dest);
    copy_entry(&mut project.source_graphics, &from.source_graphics, source, dest);
    copy_entry(&mut project.narrations, &from.narrations, source, dest);
}

pub(super) fn project_bin(target: &Project, from: &Project, id: BinId) -> Result<Project> {
    project_transfer(target, from, id, false, &mut Vec::new())
}

/// Start from target (published baseline for Save, working state for Discard).
/// Copy the selected record and its direct items, preserving nested-bin contents.
pub(super) fn project_transfer(
    target: &Project,
    from: &Project,
    id: BinId,
    saving: bool,
    copies: &mut Vec<filmcraft_format::project_folder::BinCopy>,
) -> Result<Project> {
    let old = Index::new(&target.root);
    let new = Index::new(&from.root);
    let dest = old.bins.get(&id).copied();
    let src = new.bins.get(&id).copied();
    if dest.is_some() && src.is_some() && dest.map(|b| b.1) != src.map(|b| b.1) {
        return Err(conflict("the bin was moved between parents"));
    }
    let dst_bin = dest.map(|b| b.0);
    let src_bin = src.map(|b| b.0);
    let mut next = target.clone();
    let ids: BTreeSet<_> = direct_items(dst_bin).chain(direct_items(src_bin)).collect();
    for item in ids {
        let before_owner = old.owners.get(&item).and_then(|owners| owners.first()).copied();
        let after_owner = new.owners.get(&item).and_then(|owners| owners.first()).copied();
        let sequence = target.sequence(item).is_some() || from.sequence(item).is_some();
        if sequence && target.items.contains_key(&item) && from.items.contains_key(&item) && before_owner != after_owner {
            if old.owners.get(&item).is_some_and(|owners| owners.len() > 1) || new.owners.get(&item).is_some_and(|owners| owners.len() > 1) {
                return Err(conflict("ambiguous sequence ownership"));
            }
            if after_owner == Some(id) {
                if let Some(other) = before_owner {
                    next.next_id = next.next_id.max(from.next_id);
                    if next.next_id >= Project::MAX_NEXT_ID {
                        return Err(conflict("project ID limit reached"));
                    }
                    let copy = ItemId(next.alloc_id());
                    copy_item(&mut next, target, item, copy);
                    let duplicate = next.item_mut(copy).ok_or_else(|| conflict("missing source sequence"))?;
                    duplicate.id = copy;
                    duplicate.created = copy.0;
                    duplicate.name = format!("{} (Copy)", duplicate.name);
                    let bin = next.root.find_bin_mut(other).ok_or_else(|| conflict("missing source bin"))?;
                    for entry in &mut bin.children {
                        if *entry == BinEntry::Item(item) {
                            *entry = BinEntry::Item(copy);
                        }
                    }
                    copies.push(filmcraft_format::project_folder::BinCopy { bin: other.0, original: item.0, copy: copy.0 });
                }
                copy_item(&mut next, from, item, item);
            } else if before_owner == Some(id) {
                // Removing a bin entry must not remove a sequence used by a master.
                // On discard the published item also rolls back any destination trim.
                if !saving {
                    copy_item(&mut next, from, item, item);
                }
            }
            continue;
        }
        if (target.items.contains_key(&item) && !old.owners.get(&item).is_some_and(|bins| bins.len() == 1 && bins.contains(&id)))
            || (from.items.contains_key(&item) && !new.owners.get(&item).is_some_and(|bins| bins.len() == 1 && bins.contains(&id)))
        {
            return Err(conflict("an item was moved between bins or is shared with unfiled work"));
        }
        copy_item(&mut next, from, item, item);
    }
    let old_children = dst_bin.into_iter().flat_map(members).filter(|m| matches!(m, Member::Bin(_))).collect::<BTreeSet<_>>();
    let new_children = src_bin.into_iter().flat_map(members).filter(|m| matches!(m, Member::Bin(_))).collect::<BTreeSet<_>>();
    for child in old_children.difference(&new_children) {
        if let Member::Bin(k) = child
            && (new.bins.contains_key(k) || old.bins.get(k).is_some_and(|(b, _)| !b.children.is_empty()))
        {
            return Err(conflict("a child bin was moved or its contents also need saving"));
        }
    }
    if let Some(source) = src_bin {
        let mut replacement = Bin { id, name: source.name.clone(), children: Vec::with_capacity(source.children.len()) };
        for entry in &source.children {
            replacement.children.push(match entry {
                BinEntry::Item(i) => BinEntry::Item(*i),
                BinEntry::Bin(b) => match old.bins.get(&b.id) {
                    Some((existing, Some(parent))) if *parent == id => BinEntry::Bin((*existing).clone()),
                    Some(_) => return Err(conflict("a child bin was moved between parents")),
                    None => BinEntry::Bin(Bin { id: b.id, name: b.name.clone(), children: Vec::new() }),
                },
            });
        }
        if let Some(bin) = next.root.find_bin_mut(id) {
            *bin = replacement;
        } else {
            let parent = src.and_then(|b| b.1).and_then(|p| next.root.find_bin_mut(p)).ok_or_else(|| conflict("save the parent bin first"))?;
            // Insert relative to the closest surviving following sibling. Other pending
            // rearrangements in the parent must not leak into this save.
            let siblings = new.bins.get(&parent.id).ok_or_else(|| conflict("missing parent bin"))?.0;
            let at = members(siblings)
                .skip_while(|m| *m != Member::Bin(id))
                .skip(1)
                .find_map(|m| members(parent).position(|old| old == m))
                .unwrap_or(parent.children.len());
            parent.children.insert(at, BinEntry::Bin(replacement));
        }
    } else if dst_bin.is_some() {
        next.root.remove_bin(id).ok_or_else(|| conflict("cannot remove the project root"))?;
    }

    for item in next.items.values() {
        let mut missing = false;
        item.visit_source_items(|source| missing |= !next.items.contains_key(&source));
        if missing {
            return Err(conflict("a required project item would be missing"));
        }
    }
    if target.luts != from.luts {
        for seq in next.sequences().filter_map(|item| item.as_sequence()) {
            let tracks = seq.all_tracks().chain(&seq.submix_tracks);
            let effects = tracks.flat_map(|track| track.effects.iter().chain(track.items.iter().flat_map(|clip| &clip.effects))).chain(&seq.master_effects);
            validate_luts(&next, effects)?;
        }
        validate_luts(&next, next.source_graphics.values().flat_map(|g| &g.layers))?;
    }
    if next.nest_cycle().is_some() {
        return Err(conflict("the combined timelines would form a cycle"));
    }
    next.next_id = next.next_id.max(next.max_id().saturating_add(1));
    Ok(next)
}

fn validate_luts<'a>(project: &Project, effects: impl Iterator<Item = &'a filmcraft_project::EffectInstance>) -> Result<()> {
    use filmcraft_project::ParamValue;
    for effect in effects.filter(|effect| effect.effect == "lumetri") {
        for name in ["input_lut", "look_lut"] {
            if let Some(param) = effect.params.get(name) {
                for value in std::iter::once(&param.value).chain(param.keyframes.iter().map(|k| &k.value)) {
                    if let ParamValue::Text(text) = value
                        && let Some(id) = text.strip_prefix("lib:")
                        && !project.luts.iter().any(|lut| lut.id == id)
                    {
                        return Err(conflict("a required project LUT would be missing"));
                    }
                }
            }
        }
    }
    Ok(())
}
