//! Mark-and-sweep garbage collection of a service store (RFC 0008 §5.8).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use rueca_segment::SegmentMeta;
use rueca_segment::org::OrgGeneration;
use rueca_storage::{CasStep, ObjectMeta, ObjectVersion, Storage, cas_update};
use serde::Serialize;

use crate::candidates::Round;
use crate::catalog::{RepoCatalog, Retention};
use crate::generation::{OrgPointer, POINTER_FILE, generation_file};
use crate::{CAS_ATTEMPTS, repo_dir, unix_secs};

const SEG_END_MAGIC: &[u8; 8] = b"RCSEGEND";
/// `footer_offset u64 | footer_len u64 | blake3(footer) [32] | end magic [8]`.
const SEG_TRAILER_LEN: u64 = 56;
const SEG_HEADER_LEN: u64 = 16;

/// What [`collect_garbage`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GcReport {
    /// Repository catalogs read (and possibly rewritten).
    pub catalogs: usize,
    /// Refs expired by retention, as `repo@ref`.
    pub refs_expired: Vec<String>,
    /// Catalogs rewritten (expiry or trimming).
    pub catalogs_rewritten: usize,
    /// Organization generations marked, as `org@n`.
    pub generations: Vec<String>,
    /// Objects of immutable prefixes listed.
    pub listed: usize,
    /// Of those: live (marked).
    pub live: usize,
    /// Of those: unreferenced but younger than the grace period (kept).
    pub young: usize,
    /// Of those: unreferenced, older than the grace, recorded (or kept) as
    /// deletion candidates, not deleted this round.
    pub candidates: usize,
    /// Deleted objects, by path.
    pub deleted: Vec<String>,
    pub deleted_bytes: u64,
    /// Repository directories and organizations not swept because their
    /// catalog or pointer changed during the collection (next run).
    pub skipped: Vec<String>,
    /// Segments named by a generation whose packs could not be read: their
    /// repository's packs were all kept.
    pub unreadable: Vec<String>,
}

/// What the mark phase found live.
#[derive(Default)]
struct Marks {
    live: HashSet<String>,
    /// Packs of every catalog snapshot, by segment object path.
    packs_of: HashMap<String, Vec<String>>,
    /// Version of every catalog and pointer when marked, by path.
    versions: HashMap<String, ObjectVersion>,
    /// Prefixes kept whole (packs of an unreadable segment).
    protected: Vec<String>,
}

/// Collects garbage at time `now` (see the crate docs, *Garbage
/// collection*): expires refs per `retention`, marks everything a catalog
/// or a kept generation (current and previous) references; an unmarked
/// object of an immutable prefix older than [`Retention::grace`] becomes a
/// deletion candidate (`gc/candidates.json`) and is deleted by a later
/// collection that finds it still unreferenced at least `grace` after it
/// was first seen. A zero grace deletes in this one round instead (tests;
/// unsafe with concurrent publishers). Catalogs, pointers and `store.json`
/// are never deleted.
pub fn collect_garbage(svc: &crate::ServiceStore, retention: &Retention, now: SystemTime) -> anyhow::Result<GcReport> {
    let storage = svc.storage.as_ref();
    let mut objects = storage.list("repos/")?;
    objects.extend(storage.list("orgs/")?);
    let mut report = GcReport::default();
    let mut marks = Marks::default();
    let now_s = unix_secs(now);
    for o in objects.iter().filter(|o| is_catalog(&o.path)) {
        mark_catalog(storage, &o.path, retention, now_s, &mut marks, &mut report)?;
    }
    let paths: BTreeMap<&str, u64> = objects.iter().map(|o| (o.path.as_str(), o.size)).collect();
    for o in objects.iter().filter(|o| is_pointer(&o.path)) {
        mark_org(storage, &o.path, &paths, &mut marks, &mut report)?;
    }
    if retention.grace.is_zero() {
        sweep(storage, &objects, &marks, now, &mut report)?;
    } else {
        sweep_rounds(storage, &objects, &marks, retention.grace, now, &mut report)?;
    }
    tracing::info!(deleted = report.deleted.len(), live = report.live, young = report.young, "collected garbage");
    Ok(report)
}

/// `repos/<repo-dir>/catalog.json`.
fn is_catalog(path: &str) -> bool {
    matches!(path.split('/').collect::<Vec<_>>().as_slice(), ["repos", _, "catalog.json"])
}

/// `orgs/<name>/org.json`.
fn is_pointer(path: &str) -> bool {
    matches!(path.split('/').collect::<Vec<_>>().as_slice(), ["orgs", _, POINTER_FILE])
}

/// The catalog or pointer that owns an immutable object; `None` for an
/// object outside the immutable prefixes (never swept).
fn owner_of(path: &str) -> Option<String> {
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["repos", rd, "segments" | "packs", _] => Some(format!("repos/{rd}/catalog.json")),
        ["orgs", name, "generations" | "index" | "profiles" | "links", _] => {
            Some(format!("orgs/{name}/{POINTER_FILE}"))
        }
        _ => None,
    }
}

/// Expires and trims one catalog (CAS), then marks its snapshots.
fn mark_catalog(
    storage: &dyn Storage,
    path: &str,
    retention: &Retention,
    now: u64,
    marks: &mut Marks,
    report: &mut GcReport,
) -> anyhow::Result<()> {
    let mut expired = Vec::new();
    let out = cas_update(storage, path, CAS_ATTEMPTS, |cur: Option<RepoCatalog>| {
        let mut c = cur.with_context(|| format!("{path} vanished"))?;
        let (exp, changed) = c.apply_retention(retention, now);
        expired = exp;
        Ok(if changed { CasStep::Write(c) } else { CasStep::Unchanged(c) })
    })?;
    report.catalogs += 1;
    report.catalogs_rewritten += usize::from(out.written);
    let c = out.value;
    report.refs_expired.extend(expired.iter().map(|r| format!("{}@{r}", c.repo)));
    let rd = path.trim_start_matches("repos/").trim_end_matches("/catalog.json");
    for s in c.snapshots() {
        let seg = format!("repos/{rd}/segments/{}.rcseg", s.segment);
        let packs: Vec<String> = s.packs.iter().map(|p| format!("repos/{rd}/packs/{p}.rcpack")).collect();
        marks.live.extend(packs.iter().cloned());
        marks.live.insert(seg.clone());
        marks.packs_of.insert(seg, packs);
    }
    marks.versions.insert(path.to_string(), out.version);
    Ok(())
}

/// Marks the current and previous generation of one organization and
/// everything they name.
fn mark_org(
    storage: &dyn Storage,
    pointer: &str,
    paths: &BTreeMap<&str, u64>,
    marks: &mut Marks,
    report: &mut GcReport,
) -> anyhow::Result<()> {
    let Some(v) = storage.get(pointer)? else { return Ok(()) };
    marks.versions.insert(pointer.to_string(), v.version);
    let ptr: OrgPointer = serde_json::from_slice(&v.value).with_context(|| format!("{pointer} is not valid"))?;
    let prefix = pointer.trim_end_matches(POINTER_FILE);
    for n in [Some(ptr.generation), ptr.generation.checked_sub(1).filter(|n| *n > 0)].into_iter().flatten() {
        let path = format!("{prefix}{}", generation_file(n));
        let Some(gv) = storage.get(&path)? else { continue };
        // A generation that cannot be read stops the collection: deleting
        // without knowing what it names is never safe.
        let g: OrgGeneration =
            serde_json::from_slice(&gv.value).with_context(|| format!("{path} is not a valid generation"))?;
        marks.live.insert(path);
        report.generations.push(format!("{}@{n}", ptr.name));
        mark_generation(storage, prefix, &g, paths, marks, report);
    }
    Ok(())
}

fn mark_generation(
    storage: &dyn Storage,
    prefix: &str,
    g: &OrgGeneration,
    paths: &BTreeMap<&str, u64>,
    marks: &mut Marks,
    report: &mut GcReport,
) {
    for r in &g.repos {
        let rd = repo_dir(&r.repo, None);
        for e in r.entries() {
            if let Some(uuid) = &e.tantivy_segment {
                let files = format!("{prefix}index/{uuid}.");
                let named = paths.range(files.as_str()..).take_while(|(p, _)| p.starts_with(&files));
                marks.live.extend(named.map(|(p, _)| (*p).to_string()));
            }
            marks.live.extend(e.profile.iter().map(|p| format!("{prefix}profiles/{p}")));
            marks.live.extend(e.links.as_ref().and_then(|l| l.file.as_ref()).map(|f| format!("{prefix}links/{f}")));
            let seg = format!("repos/{rd}/segments/{}.rcseg", e.segment_id);
            mark_segment_packs(storage, &rd, &seg, paths, marks, report);
            marks.live.insert(seg);
        }
    }
}

/// Marks the packs of a segment a generation names: from a catalog
/// snapshot when one lists it, else from the segment's own `meta`.
fn mark_segment_packs(
    storage: &dyn Storage,
    rd: &str,
    seg: &str,
    paths: &BTreeMap<&str, u64>,
    marks: &mut Marks,
    report: &mut GcReport,
) {
    if marks.packs_of.contains_key(seg) {
        return;
    }
    let Some(&size) = paths.get(seg) else { return };
    match remote_segment_meta(storage, seg, size) {
        Ok(meta) => {
            let packs: Vec<String> = meta.packs.iter().map(|p| format!("repos/{rd}/packs/{p}.rcpack")).collect();
            marks.live.extend(packs.iter().cloned());
            marks.packs_of.insert(seg.to_string(), packs);
        }
        Err(e) => {
            tracing::warn!(segment = seg, "cannot read the packs of a generation's segment, keeping all packs: {e:#}");
            marks.protected.push(format!("repos/{rd}/packs/"));
            report.unreadable.push(seg.to_string());
        }
    }
}

/// Reads the `meta` component of a segment in the store with three range
/// reads (trailer, footer, component), checking the footer's and the
/// component's BLAKE3 digests. The bundle layout is rueca-segment's.
fn remote_segment_meta(storage: &dyn Storage, path: &str, size: u64) -> anyhow::Result<SegmentMeta> {
    if size < SEG_HEADER_LEN + SEG_TRAILER_LEN {
        bail!("{path}: too short for a segment");
    }
    let trailer = storage.get_range(path, size - SEG_TRAILER_LEN..size)?;
    if trailer.len() as u64 != SEG_TRAILER_LEN || &trailer[48..56] != SEG_END_MAGIC {
        bail!("{path}: bad segment trailer");
    }
    let footer_offset = le_u64(&trailer, 0)?;
    let footer_len = le_u64(&trailer, 8)?;
    if footer_offset.checked_add(footer_len) != Some(size - SEG_TRAILER_LEN) {
        bail!("{path}: bad footer bounds");
    }
    let footer = storage.get_range(path, footer_offset..footer_offset + footer_len)?;
    if blake3::hash(&footer).as_bytes()[..] != trailer[16..48] {
        bail!("{path}: footer checksum mismatch");
    }
    let (offset, len, digest) = find_component(&footer, "meta").with_context(|| format!("{path}: no meta"))?;
    let end = offset.checked_add(len).filter(|e| *e <= footer_offset).context("meta out of bounds")?;
    let bytes = storage.get_range(path, offset..end)?;
    if blake3::hash(&bytes).as_bytes() != &digest {
        bail!("{path}: meta checksum mismatch");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn le_u64(b: &[u8], at: usize) -> anyhow::Result<u64> {
    let s = b.get(at..at + 8).context("short read")?;
    Ok(u64::from_le_bytes(s.try_into()?))
}

/// `(offset, len, blake3)` of component `name` in a bundle footer:
/// `u32 count`, then per component `u16 name_len | name | u64 offset |
/// u64 len | [32] blake3`.
fn find_component(footer: &[u8], name: &str) -> Option<(u64, u64, [u8; 32])> {
    let count = u32::from_le_bytes(footer.get(..4)?.try_into().ok()?);
    let mut at = 4usize;
    for _ in 0..count {
        let name_len = usize::from(u16::from_le_bytes(footer.get(at..at + 2)?.try_into().ok()?));
        let this = footer.get(at + 2..at + 2 + name_len)?;
        at += 2 + name_len;
        let offset = u64::from_le_bytes(footer.get(at..at + 8)?.try_into().ok()?);
        let len = u64::from_le_bytes(footer.get(at + 8..at + 16)?.try_into().ok()?);
        let digest: [u8; 32] = footer.get(at + 16..at + 48)?.try_into().ok()?;
        at += 48;
        if this == name.as_bytes() {
            return Some((offset, len, digest));
        }
    }
    None
}

/// Whether an object is older than `grace` at `now` (no timestamp, or one
/// in the future: no).
fn older_than(meta: &ObjectMeta, grace: Duration, now: SystemTime) -> bool {
    meta.last_modified.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= grace)
}

/// Whether an object is live (marked or under a protected prefix).
fn is_live(marks: &Marks, path: &str) -> bool {
    marks.live.contains(path) || marks.protected.iter().any(|p| path.starts_with(p.as_str()))
}

/// Grace 0: deletes every unmarked object of an immutable prefix in this
/// round. Right before each deletion the object's catalog or pointer is
/// read again (a conditional GET); once it changed since the mark, its
/// objects are kept until the next run.
fn sweep(
    storage: &dyn Storage,
    objects: &[ObjectMeta],
    marks: &Marks,
    now: SystemTime,
    report: &mut GcReport,
) -> anyhow::Result<()> {
    let mut changed = BTreeSet::new();
    for o in objects {
        let Some(owner) = owner_of(&o.path) else { continue };
        report.listed += 1;
        if is_live(marks, &o.path) {
            report.live += 1;
        } else if !older_than(o, Duration::ZERO, now) {
            report.young += 1;
        } else if changed.contains(&owner) || !owner_unchanged(storage, &owner, marks)? {
            changed.insert(owner);
        } else {
            storage.delete(&o.path)?;
            report.deleted.push(o.path.clone());
            report.deleted_bytes += o.size;
        }
    }
    report.skipped = changed.into_iter().collect();
    Ok(())
}

/// Grace > 0: two-round deletion through `gc/candidates.json`
/// ([`crate::candidates`]).
fn sweep_rounds(
    storage: &dyn Storage,
    objects: &[ObjectMeta],
    marks: &Marks,
    grace: Duration,
    now: SystemTime,
    report: &mut GcReport,
) -> anyhow::Result<()> {
    let mut unreferenced = Vec::new();
    for o in objects.iter().filter(|o| owner_of(&o.path).is_some()) {
        report.listed += 1;
        if is_live(marks, &o.path) {
            report.live += 1;
        } else if !older_than(o, grace, now) {
            report.young += 1;
        } else {
            unreferenced.push(o);
        }
    }
    // Whole seconds, rounded up: a sub-second grace is never zero.
    let grace_s = grace.as_secs() + u64::from(grace.subsec_nanos() > 0);
    let deleted = Round { storage, now: unix_secs(now), grace: grace_s }.run(&unreferenced)?;
    let sizes: HashMap<&str, u64> = objects.iter().map(|o| (o.path.as_str(), o.size)).collect();
    report.candidates = unreferenced.len().saturating_sub(deleted.len());
    report.deleted_bytes = deleted.iter().filter_map(|p| sizes.get(p.as_str())).sum();
    report.deleted = deleted;
    Ok(())
}

/// Whether catalog or pointer `owner` still has the version it was marked
/// at (or is still absent).
fn owner_unchanged(storage: &dyn Storage, owner: &str, marks: &Marks) -> anyhow::Result<bool> {
    match marks.versions.get(owner) {
        Some(v) => Ok(storage.get_if_changed(owner, v)?.is_none()),
        None => Ok(storage.get(owner)?.is_none()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_classes() {
        assert!(is_catalog("repos/x-1/catalog.json"));
        assert!(!is_catalog("repos/x-1/segments/catalog.json"));
        assert!(is_pointer("orgs/acme/org.json"));
        assert_eq!(owner_of("repos/x-1/packs/p.rcpack").as_deref(), Some("repos/x-1/catalog.json"));
        assert_eq!(owner_of("orgs/acme/index/u.idx").as_deref(), Some("orgs/acme/org.json"));
        for never in ["store.json", "repos/x-1/catalog.json", "orgs/acme/org.json", "jobs/x.json", "repos/x/a/b/c"] {
            assert_eq!(owner_of(never), None, "{never}");
        }
    }

    #[test]
    fn footer_lookup() {
        let mut f = Vec::new();
        f.extend_from_slice(&2u32.to_le_bytes());
        for (name, off) in [("nodes", 16u64), ("meta", 64)] {
            f.extend_from_slice(&(name.len() as u16).to_le_bytes());
            f.extend_from_slice(name.as_bytes());
            f.extend_from_slice(&off.to_le_bytes());
            f.extend_from_slice(&10u64.to_le_bytes());
            f.extend_from_slice(&[7u8; 32]);
        }
        assert_eq!(find_component(&f, "meta"), Some((64, 10, [7u8; 32])));
        assert_eq!(find_component(&f, "files"), None);
        assert_eq!(find_component(&f[..20], "meta"), None);
    }
}
