//! Retire project versions before collecting their exclusively owned records.
//! The immutable manifest chain remains available to authenticate history and
//! locate move backups. Only payloads named by that chain can be collected.
use super::{MANIFEST_LIMIT, Manifest, check_hash, check_manifest, digest, invalid, read_member, recovery, revisions};
use crate::MAX_PROJECT_BYTES;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

pub(super) mod background;

const INDEX: &str = "recovery-retention.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetentionPolicy {
    pub enabled: bool,
    pub max_bin_backups: u32,
    pub versions_per_bin: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self { enabled: true, max_bin_backups: 2_000, versions_per_bin: 2_000 }
    }
}

impl RetentionPolicy {
    fn validate(self) -> io::Result<()> {
        if self.max_bin_backups == 0 || self.versions_per_bin == 0 {
            return Err(invalid("retention counts must be positive"));
        }
        Ok(())
    }
}

#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Catalog {
    through: u64,
    anchor: Option<String>,
    retained: BTreeSet<String>,
    pins: BTreeSet<String>,
    policy: RetentionPolicy,
    pending: BTreeSet<String>,
    processed_backups: u64,
    ordinary_backups: u64,
}

impl Catalog {
    pub(super) fn read(folder: &Path) -> io::Result<Self> {
        match read_member(&folder.join(INDEX), MANIFEST_LIMIT) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    fn outstanding(&self, total: u64) -> u64 {
        self.ordinary_backups.saturating_add(total.saturating_sub(self.processed_backups))
    }
    pub(super) fn due(&self, total: u64) -> bool {
        self.policy.enabled && (!self.pending.is_empty() || self.outstanding(total) > u64::from(self.policy.max_bin_backups))
    }
    fn write(&self, folder: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid("retention catalog exceeds size limit"));
        }
        crate::atomic_write_no_follow(&folder.join(INDEX), &bytes)
    }
    pub(super) fn available(&self, number: u64, hash: &str) -> bool {
        self.anchor.is_none() || number > self.through || self.retained.contains(hash)
    }
}

/// Counts refer to historical saved projects. Protected versions are exempt
/// from the ordinary backup count.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPreview {
    pub revision: String,
    pub save_number: u64,
    pub due: bool,
    pub retire: Vec<String>,
    pub retained: Vec<String>,
    pub bin_backups: u64,
    pub retained_bin_backups: u64,
    pub records: usize,
}

fn payloads(m: &Manifest) -> impl Iterator<Item = &String> {
    std::iter::once(&m.envelope).chain(m.bins.iter().chain(&m.items).map(|(_, h)| h))
}

fn backup_manifest(folder: &Path, hash: &str) -> io::Result<Manifest> {
    check_hash(hash)?;
    let bytes = read_member(&folder.join(hash), MANIFEST_LIMIT)?;
    if digest(&bytes) != hash {
        return Err(invalid("damaged protected manifest"));
    }
    let m = serde_json::from_slice(&bytes)?;
    check_manifest(&m)?;
    Ok(m)
}

struct Plan {
    preview: RetentionPreview,
    garbage: Vec<String>,
    total: u64,
}

fn plan(
    folder: &Path,
    state: &Catalog,
    policy: RetentionPolicy,
    expected: &str,
    verified_head: Option<&Manifest>,
    mut check_progress: impl FnMut() -> io::Result<()>,
) -> io::Result<Plan> {
    policy.validate()?;
    let from_disk;
    let head = if let Some(head) = verified_head {
        head
    } else {
        from_disk = revisions::read_base(folder, expected)?.1;
        &from_disk
    };
    let number = head.revision.as_ref().map_or(0, |r| r.number);
    let mut keep = state.pins.clone();
    keep.insert(expected.to_owned());
    if let Some(r) = &head.revision {
        keep.extend(r.active_move_bases.iter().cloned());
    }
    let mut known = BTreeSet::new();
    let mut protected = BTreeSet::new();
    let mut eligible = BTreeSet::new();
    let mut move_points = BTreeSet::new();
    let total = head.revision.as_ref().map_or(0, |r| r.total_bin_backups);
    let mut selected = 0u64;
    let target = u64::from(policy.max_bin_backups - policy.max_bin_backups / 10);
    let mut by_bin = BTreeMap::<Option<u64>, u32>::new();
    let mut anchor_found = state.anchor.is_none();
    // Discover the complete set of protected move roots before inspecting any
    // retired payload. Standalone departure snapshots are outside the parent chain.
    recovery::walk_from(folder, expected.to_owned(), head.clone(), |hash, m| {
        check_progress()?;
        known.insert(hash.to_owned());
        let n = m.revision.as_ref().map_or(0, |r| r.number);
        anchor_found |= state.anchor.as_deref() == Some(hash) && n == state.through;
        if let Some(rev) = &m.revision {
            for b in &rev.moves {
                keep.insert(b.saved_source.clone());
                move_points.insert(b.snapshot.clone());
            }
        }
        Ok(false)
    })?;
    if !anchor_found || !keep.is_subset(&known) || !state.retained.is_subset(&known) {
        return Err(invalid("retention catalog does not match project history"));
    }
    for hash in move_points {
        check_progress()?;
        if known.contains(&hash) {
            keep.insert(hash);
        } else {
            let m = backup_manifest(folder, &hash)?;
            protected.insert(hash);
            protected.extend(payloads(&m).cloned());
        }
    }
    let mut retire = Vec::new();
    // A save backs up the *previous* state of the bins it changed. The
    // child names those bins even if a bin was removed by that save.
    let mut backed_up = BTreeSet::new();
    recovery::walk_from(folder, expected.to_owned(), head.clone(), |hash, m| {
        check_progress()?;
        let n = m.revision.as_ref().map_or(0, |r| r.number);
        let available = state.available(n, hash);
        if available && !keep.contains(hash) {
            let mut scopes: Vec<_> = backed_up.iter().copied().map(Some).collect();
            if scopes.is_empty() {
                scopes.push(None);
            }
            let weight = backed_up.len() as u64;
            let room = selected.saturating_add(weight) <= target || selected == 0 && weight > target;
            if room && scopes.iter().all(|id| by_bin.get(id).copied().unwrap_or(0) < policy.versions_per_bin) {
                selected += weight;
                for id in scopes {
                    *by_bin.entry(id).or_default() += 1;
                }
                keep.insert(hash.to_owned());
            }
        }
        backed_up = m.revision.as_ref().map(|r| r.backed_up_bins.clone()).unwrap_or_default();
        if keep.contains(hash) {
            protected.extend(payloads(m).cloned());
        } else {
            eligible.extend(payloads(m).filter(|h| available || state.pending.contains(*h)).cloned());
            if available {
                retire.push(hash.to_owned());
            }
        }
        Ok(false)
    })?;
    // All manifest files remain walkable, including a digest shared by roles.
    eligible.retain(|hash| !protected.contains(hash) && !known.contains(hash));
    // Only candidate names from authenticated manifests may reach unlink.
    // The catalog can request a retry but cannot introduce an arbitrary path.
    let garbage: Vec<_> = eligible.into_iter().collect();
    for hash in &garbage {
        check_hash(hash)?;
    }
    // The publishing writer already verified the live payloads under this
    // lock. Recheck only records exclusive to retained history on that path.
    if let Some(manifest) = verified_head {
        for hash in payloads(manifest) {
            protected.remove(hash);
        }
    }
    for hash in protected {
        check_progress()?;
        check_hash(&hash)?;
        let data = read_member(&folder.join(&hash), MAX_PROJECT_BYTES)?;
        if digest(&data) != hash {
            return Err(invalid("damaged retained recovery record"));
        }
    }
    Ok(Plan {
        preview: RetentionPreview {
            revision: expected.to_owned(),
            save_number: number,
            due: policy.enabled && state.outstanding(total) > u64::from(policy.max_bin_backups),
            retire,
            retained: keep.into_iter().collect(),
            bin_backups: state.outstanding(total),
            retained_bin_backups: selected,
            records: garbage.len(),
        },
        garbage,
        total,
    })
}

pub fn preview_retention(folder: &Path, expected: &str, policy: RetentionPolicy) -> io::Result<RetentionPreview> {
    let _guard = revisions::writer(folder)?;
    Ok(plan(folder, &Catalog::read(folder)?, policy, expected, None, || Ok(()))?.preview)
}

fn publish_visibility(folder: &Path, expected: &str, state: &mut Catalog, plan: &Plan) -> io::Result<()> {
    state.processed_backups = plan.total;
    state.ordinary_backups = plan.preview.retained_bin_backups;
    state.anchor = Some(expected.to_owned());
    state.through = plan.preview.save_number;
    state.retained = plan.preview.retained.iter().cloned().collect();
    // Publish visibility first. An interrupted unlink leaves reclaimable bytes,
    // and a retry recomputes them from managed history under the same lock.
    state.pending = plan.garbage.iter().cloned().collect();
    state.write(folder)?;
    #[cfg(unix)]
    fs::File::open(folder)?.sync_all()?;
    Ok(())
}

fn apply_locked(folder: &Path, expected: &str, policy: RetentionPolicy, mut state: Catalog, verified_head: Option<&Manifest>) -> io::Result<RetentionPreview> {
    let plan = plan(folder, &state, policy, expected, verified_head, || Ok(()))?;
    publish_visibility(folder, expected, &mut state, &plan)?;
    if !plan.garbage.is_empty() {
        for hash in plan.garbage {
            let path = folder.join(&hash);
            match read_member(&path, MAX_PROJECT_BYTES) {
                Ok(data) if digest(&data) == hash => fs::remove_file(path)?,
                Ok(_) => return Err(invalid("damaged retired record; cleanup stopped")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        #[cfg(unix)]
        fs::File::open(folder)?.sync_all()?;
        state.pending.clear();
        state.write(folder)?;
    }
    Ok(plan.preview)
}

pub fn prune_history(folder: &Path, expected: &str, policy: RetentionPolicy) -> io::Result<RetentionPreview> {
    let _guard = revisions::writer(folder)?;
    apply_locked(folder, expected, policy, Catalog::read(folder)?, None)
}

pub fn keep_version(folder: &Path, expected: &str, version: &str, keep: bool) -> io::Result<()> {
    let _guard = revisions::writer(folder)?;
    revisions::read_base(folder, expected)?;
    let mut state = Catalog::read(folder)?;
    if keep {
        let available = recovery::walk(folder, |hash, m| Ok(hash == version && state.available(m.revision.as_ref().map_or(0, |r| r.number), hash)))?.is_some();
        if !available {
            return Err(invalid("version is outside retained project history"));
        }
        state.pins.insert(version.to_owned());
    } else {
        state.pins.remove(version);
    }
    state.write(folder)
}

pub fn configure_retention(folder: &Path, expected: &str, policy: RetentionPolicy) -> io::Result<()> {
    let _guard = revisions::writer(folder)?;
    revisions::read_base(folder, expected)?;
    policy.validate()?;
    let mut state = Catalog::read(folder)?;
    state.policy = policy;
    state.write(folder)
}

pub fn retention_policy(folder: &Path) -> io::Result<RetentionPolicy> {
    Ok(Catalog::read(folder)?.policy)
}

pub(super) fn after_publication(folder: &Path, expected: &str, head: &Manifest) -> io::Result<()> {
    let Some(rev) = &head.revision else { return Ok(()) };
    let state = Catalog::read(folder)?;
    let policy = state.policy;
    if policy.enabled
        && (!state.pending.is_empty() || !rev.backed_up_bins.is_empty() && state.outstanding(rev.total_bin_backups) > u64::from(policy.max_bin_backups))
    {
        apply_locked(folder, expected, policy, state, Some(head))?;
    }
    Ok(())
}
