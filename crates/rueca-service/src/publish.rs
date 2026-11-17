//! Publishing a local snapshot (RFC 0008 §7.9).

use std::collections::HashSet;

use anyhow::{Context, bail, ensure};
use rueca_segment::SegmentMeta;
use rueca_storage::{CasStep, cas_update};
use serde::Serialize;

use crate::candidates::rescue;
use crate::catalog::{RefKind, RepoCatalog, Retention, SnapshotEntry};
use crate::{CAS_ATTEMPTS, ServiceStore, put_file_once, repo_dir};

/// What [`publish_snapshot`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PublishReport {
    pub repo: String,
    pub repo_dir: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub commit: String,
    pub segment: String,
    /// The ref already had this commit: the catalog was not changed.
    pub duplicate: bool,
    pub packs_uploaded: usize,
    /// Packs the repository directory already held.
    pub packs_present: usize,
    pub segment_uploaded: bool,
    pub bytes_uploaded: u64,
    /// Objects uploaded again after the catalog CAS because a concurrent
    /// garbage collection had removed them.
    pub repaired: usize,
    /// Segments of the ref that retention dropped from the catalog.
    pub trimmed: Vec<String>,
    /// Catalog writes lost to other publishers before this one.
    pub conflicts: u32,
}

/// Publishes local segment `local_segment_id` (a segment of the mirror,
/// [`ServiceStore::local`]) as the newest snapshot of ref `ref_name`
/// (`kind`) of repository `repo`: uploads its packs and segment with
/// `Create`, then updates the repository catalog by compare-and-swap,
/// trimming the ref to `retention`. A clean snapshot whose commit the ref
/// already has is a duplicate (`duplicate: true`, catalog unchanged). See
/// the crate docs, *Sequences*.
pub fn publish_snapshot(
    svc: &ServiceStore,
    repo: &str,
    ref_name: &str,
    kind: RefKind,
    local_segment_id: &str,
    retention: &Retention,
) -> anyhow::Result<PublishReport> {
    ensure!(!ref_name.trim().is_empty(), "the ref name is empty");
    let meta = svc
        .local
        .open_segment(local_segment_id)
        .with_context(|| format!("opening local segment {local_segment_id}"))?
        .meta()
        .clone();
    if meta.snapshot.repo != repo {
        bail!("segment {local_segment_id} is a snapshot of `{}`, not of `{repo}`", meta.snapshot.repo);
    }
    if meta.packs.is_empty() && meta.file_count > 0 {
        bail!("segment {local_segment_id} does not list its packs (written before RFC 0008); re-index it");
    }
    let rd = repo_dir(repo, None);
    let mut report = PublishReport {
        repo: repo.to_string(),
        repo_dir: rd.clone(),
        ref_name: ref_name.to_string(),
        commit: meta.snapshot.commit.clone(),
        segment: meta.id.clone(),
        ..PublishReport::default()
    };
    // Take back what the snapshot reuses before a collection can delete it.
    let mut objects: Vec<String> =
        meta.packs.iter().map(|p| ServiceStore::repo_path(&rd, &format!("packs/{p}.rcpack"))).collect();
    objects.push(ServiceStore::repo_path(&rd, &format!("segments/{}.rcseg", meta.id)));
    rescue(svc.storage.as_ref(), &objects)?;
    let first = upload_objects(svc, &rd, &meta)?;
    report.packs_uploaded = first.packs_uploaded;
    report.packs_present = first.packs_present;
    report.segment_uploaded = first.segment_uploaded;
    report.bytes_uploaded = first.bytes;

    let entry = snapshot_entry(&meta);
    let mut trimmed = Vec::new();
    let path = ServiceStore::repo_path(&rd, "catalog.json");
    let out = cas_update(svc.storage.as_ref(), &path, CAS_ATTEMPTS, |cur: Option<RepoCatalog>| {
        let mut c = cur.unwrap_or_else(|| RepoCatalog::new(repo));
        if c.repo != repo {
            bail!("{path} is the catalog of `{}`, not of `{repo}` (repository directory collision)", c.repo);
        }
        if !entry.dirty && c.has_snapshot(ref_name, &entry.commit) {
            return Ok(CasStep::Unchanged(c));
        }
        trimmed = c.insert(ref_name, kind, entry.clone(), retention);
        Ok(CasStep::Write(c))
    })?;
    report.duplicate = !out.written;
    report.conflicts = out.conflicts;
    if out.written {
        report.trimmed = trimmed.into_iter().map(|s| s.segment).collect();
        // A collection that marked before the CAS may have removed a pack
        // this snapshot reuses: put back what is missing.
        let again = upload_objects(svc, &rd, &meta)?;
        report.repaired = again.packs_uploaded + usize::from(again.segment_uploaded);
        if report.repaired > 0 {
            tracing::warn!(repo, segment = %meta.id, repaired = report.repaired, "re-uploaded objects removed during publish");
        }
    }
    tracing::info!(repo, ref_name, segment = %meta.id, duplicate = report.duplicate, "published snapshot");
    Ok(report)
}

fn snapshot_entry(meta: &SegmentMeta) -> SnapshotEntry {
    SnapshotEntry {
        commit: meta.snapshot.commit.clone(),
        segment: meta.id.clone(),
        indexed_at: meta.snapshot.indexed_at,
        rueca_version: meta.snapshot.rueca_version.clone(),
        bytes: meta.bytes,
        packs: meta.packs.clone(),
        vectors: None,
        dirty: meta.snapshot.dirty,
    }
}

#[derive(Default)]
struct Uploaded {
    packs_uploaded: usize,
    packs_present: usize,
    segment_uploaded: bool,
    bytes: u64,
}

/// Uploads the packs of `meta` the repository directory does not list,
/// then its segment, with `Create`.
fn upload_objects(svc: &ServiceStore, rd: &str, meta: &SegmentMeta) -> anyhow::Result<Uploaded> {
    let storage = svc.storage.as_ref();
    let mut up = Uploaded::default();
    let present: HashSet<String> =
        storage.list(&ServiceStore::repo_path(rd, "packs/"))?.into_iter().map(|m| m.path).collect();
    for pack in &meta.packs {
        let path = ServiceStore::repo_path(rd, &format!("packs/{pack}.rcpack"));
        let src = svc.local.dir.join("packs").join(format!("{pack}.rcpack"));
        if present.contains(&path) || !put_file_once(storage, &path, &src)? {
            up.packs_present += 1;
            continue;
        }
        up.packs_uploaded += 1;
        up.bytes += std::fs::metadata(&src).map_or(0, |m| m.len());
    }
    let path = ServiceStore::repo_path(rd, &format!("segments/{}.rcseg", meta.id));
    if storage.list(&path)?.is_empty() {
        let src = svc.local.segment_path(&meta.id);
        up.segment_uploaded = put_file_once(storage, &path, &src)?;
        if up.segment_uploaded {
            up.bytes += meta.bytes;
        }
    }
    Ok(up)
}
