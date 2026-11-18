//! Organization generations in the service store (RFC 0008 §5.4).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use bytes::Bytes;
use rueca_segment::org::{OrgEntry, OrgGeneration, current_generation, org_dir};
use rueca_storage::{CasStep, ObjectVersion, PutError, Storage, cas_update};
use serde::{Deserialize, Serialize};

use crate::candidates::rescue;
use crate::pull::Fetcher;
use crate::{CAS_ATTEMPTS, ServiceStore, put_file_once, repo_dir, write_atomic};

/// `orgs/<name>/org.json`, the generation pointer (the local store's
/// format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OrgPointer {
    pub name: String,
    pub generation: u64,
}

pub(crate) const POINTER_FILE: &str = "org.json";

/// `generations/<n:010>.json`, as in the local store.
pub(crate) fn generation_file(n: u64) -> String {
    format!("generations/{n:010}.json")
}

/// Another updater published the generation first: nothing of this one
/// was installed. Pull the store's generation, rebuild on top of it and
/// publish again.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("organization `{org}`: generation {generation} was not published: {reason}")]
pub struct Conflict {
    pub org: String,
    pub generation: u64,
    pub reason: String,
}

/// What [`publish_generation`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GenerationReport {
    pub org: String,
    pub generation: u64,
    /// The generation the build started from (`None`: the first).
    pub base: Option<u64>,
    /// Whether this call moved `org.json` (false: it already named the
    /// generation).
    pub published: bool,
    /// `index/`, `profiles/` and `links/` objects uploaded.
    pub files_uploaded: usize,
    pub bytes_uploaded: u64,
    /// Pointer writes lost to other writers before this one.
    pub conflicts: u32,
}

/// What [`pull_generation`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PullGenerationReport {
    pub org: String,
    pub generation: u64,
    /// Version of `org.json` read: pass it to the next call.
    pub version: ObjectVersion,
    pub repos: usize,
    pub segments_downloaded: usize,
    pub packs_downloaded: usize,
    /// `index/`, `profiles/` and `links/` files downloaded.
    pub files_downloaded: usize,
    pub bytes_downloaded: u64,
}

/// Publishes the mirror's current generation of the organization, built
/// by [`rueca_segment::org::update_org_with`] on the mirror (see the crate
/// docs, *Sequences*). Fails with [`Conflict`] when another updater
/// published that generation number with other content, or moved
/// `org.json` past the generation this build started from.
pub fn publish_generation(svc: &ServiceStore) -> anyhow::Result<GenerationReport> {
    let org = svc.org.as_str();
    let Some(g) = current_generation(&svc.local.dir, org)? else {
        bail!(
            "organization `{org}` has no generation in the mirror {}; build it with update_org_with first",
            svc.local.dir.display()
        );
    };
    let n = g.generation;
    let base = n.checked_sub(1).filter(|b| *b > 0);
    let local = org_dir(&svc.local.dir, org);
    let bytes = fs::read(local.join(generation_file(n)))?;
    rescue(svc.storage.as_ref(), &named_objects(svc, &g, &local)?)?;
    check_segments_published(svc, &g)?;
    put_generation(svc, &g, bytes)?;
    let mut report = GenerationReport { org: org.to_string(), generation: n, base, ..GenerationReport::default() };
    upload_org_files(svc, &g, &local, &mut report)?;
    let path = svc.org_path(POINTER_FILE);
    let out = cas_update(svc.storage.as_ref(), &path, CAS_ATTEMPTS, |cur: Option<OrgPointer>| {
        let next = OrgPointer { name: org.to_string(), generation: n };
        match cur {
            None => Ok(CasStep::Write(next)),
            Some(p) if p.generation == n => Ok(CasStep::Unchanged(p)),
            Some(p) if Some(p.generation) == base => Ok(CasStep::Write(next)),
            Some(p) => Err(Conflict {
                org: org.to_string(),
                generation: n,
                reason: format!("the store is at generation {}, this build started from {base:?}", p.generation),
            }
            .into()),
        }
    })?;
    report.published = out.written;
    report.conflicts = out.conflicts;
    tracing::info!(org, generation = n, published = report.published, "published generation");
    Ok(report)
}

/// The files of the mirror's organization `index/` directory.
fn local_index_files(local: &Path) -> BTreeSet<String> {
    match fs::read_dir(local.join("index")) {
        Ok(rd) => rd.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Every store object generation `g` names: its organization files, and
/// each entry's segment and packs (read from the mirror's copy).
fn named_objects(svc: &ServiceStore, g: &OrgGeneration, local: &Path) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = generation_files(g, &local_index_files(local))
        .into_iter()
        .map(|(sub, file)| svc.org_path(&format!("{sub}/{file}")))
        .collect();
    for r in &g.repos {
        let rd = repo_dir(&r.repo, None);
        for e in r.entries() {
            let packs = svc.segment_packs(&e.segment_id)?;
            out.extend(packs.iter().map(|p| ServiceStore::repo_path(&rd, &format!("packs/{p}.rcpack"))));
            out.push(ServiceStore::repo_path(&rd, &format!("segments/{}.rcseg", e.segment_id)));
        }
    }
    Ok(out)
}

/// Fails unless every segment `g` names is in the store: a generation is
/// built from published snapshots only (pulled into the mirror).
fn check_segments_published(svc: &ServiceStore, g: &OrgGeneration) -> anyhow::Result<()> {
    for r in &g.repos {
        let rd = repo_dir(&r.repo, None);
        let listed: HashSet<String> =
            svc.storage.list(&ServiceStore::repo_path(&rd, "segments/"))?.into_iter().map(|m| m.path).collect();
        for e in r.entries() {
            let path = ServiceStore::repo_path(&rd, &format!("segments/{}.rcseg", e.segment_id));
            if !listed.contains(&path) {
                bail!(
                    "generation {} of `{}` names segment {} of `{}`, which is not in the store; publish or pull \
                     the repository and update the organization again",
                    g.generation,
                    svc.org,
                    e.segment_id,
                    r.repo
                );
            }
        }
    }
    Ok(())
}

/// `Create` of the generation file; an existing one must be the same
/// generation (roots aside: they are each mirror's own).
fn put_generation(svc: &ServiceStore, g: &OrgGeneration, bytes: Vec<u8>) -> anyhow::Result<()> {
    let path = svc.org_path(&generation_file(g.generation));
    match svc.storage.put_new(&path, Bytes::from(bytes)) {
        Ok(_) => Ok(()),
        Err(PutError::AlreadyExists) => {
            let theirs = read_generation(svc.storage.as_ref(), &path)?;
            if without_roots(&theirs) == without_roots(g) {
                return Ok(());
            }
            Err(Conflict {
                org: svc.org.clone(),
                generation: g.generation,
                reason: "another updater published this generation first".into(),
            }
            .into())
        }
        Err(e) => Err(anyhow::Error::from(e).context(format!("uploading {path}"))),
    }
}

fn read_generation(storage: &dyn Storage, path: &str) -> anyhow::Result<OrgGeneration> {
    let v = storage.get(path)?.with_context(|| format!("{path} is missing"))?;
    serde_json::from_slice(&v.value).with_context(|| format!("{path} is not a valid generation"))
}

fn without_roots(g: &OrgGeneration) -> OrgGeneration {
    let mut g = g.clone();
    for r in &mut g.repos {
        r.default.root.clear();
        for x in &mut r.refs {
            x.entry.root.clear();
        }
    }
    g
}

/// The organization files an entry names, as `(subdirectory, file)`;
/// `index_files` lists the mirror's (or the store's) `index/` directory.
fn entry_files(e: &OrgEntry, index_files: &BTreeSet<String>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(uuid) = &e.tantivy_segment {
        let prefix = format!("{uuid}.");
        let files = index_files.range(prefix.clone()..).take_while(|f| f.starts_with(&prefix));
        out.extend(files.map(|f| ("index", f.clone())));
    }
    out.extend(e.profile.iter().map(|p| ("profiles", p.clone())));
    out.extend(e.links.as_ref().and_then(|l| l.file.clone()).map(|f| ("links", f)));
    out
}

fn generation_files(g: &OrgGeneration, index_files: &BTreeSet<String>) -> BTreeSet<(&'static str, String)> {
    g.repos.iter().flat_map(|r| r.entries()).flat_map(|e| entry_files(e, index_files)).collect()
}

/// Uploads the `index/`, `profiles/` and `links/` files of `g` the store
/// does not hold.
fn upload_org_files(
    svc: &ServiceStore,
    g: &OrgGeneration,
    local: &Path,
    report: &mut GenerationReport,
) -> anyhow::Result<()> {
    let storage = svc.storage.as_ref();
    let present: HashSet<String> = storage.list(&svc.org_path(""))?.into_iter().map(|m| m.path).collect();
    for (sub, file) in generation_files(g, &local_index_files(local)) {
        let path = svc.org_path(&format!("{sub}/{file}"));
        let src = local.join(sub).join(&file);
        if !present.contains(&path) && put_file_once(storage, &path, &src)? {
            report.files_uploaded += 1;
            report.bytes_uploaded += fs::metadata(&src).map_or(0, |m| m.len());
        }
    }
    Ok(())
}

/// Pulls the organization's current generation into the mirror when
/// `org.json` changed since `known` (`None`: always): downloads the
/// generation, every segment and pack it names and its `index/`,
/// `profiles/` and `links/` files, rewrites its roots to the mirror's
/// synthetic roots, then writes the generation file and, last, the local
/// `org.json`, under the mirror's store lock. `Ok(None)`: unchanged, or no
/// generation published yet.
pub fn pull_generation(
    svc: &ServiceStore,
    known: Option<&ObjectVersion>,
) -> anyhow::Result<Option<PullGenerationReport>> {
    let storage = svc.storage.as_ref();
    let path = svc.org_path(POINTER_FILE);
    let got = match known {
        Some(v) => storage.get_if_changed(&path, v)?,
        None => storage.get(&path)?,
    };
    let Some(got) = got else { return Ok(None) };
    let ptr: OrgPointer = serde_json::from_slice(&got.value).with_context(|| format!("{path} is not valid"))?;
    let mut g = read_generation(storage, &svc.org_path(&generation_file(ptr.generation)))?;
    if g.generation != ptr.generation || g.name != svc.org {
        bail!("generation file {} of `{}` names generation {} of `{}`", ptr.generation, svc.org, g.generation, g.name);
    }
    let mut report = PullGenerationReport {
        org: svc.org.clone(),
        generation: g.generation,
        version: got.version,
        repos: g.repos.len(),
        ..PullGenerationReport::default()
    };
    for r in &g.repos {
        let mut fetcher = Fetcher::new(svc, &repo_dir(&r.repo, None))?;
        for e in r.entries() {
            fetcher.snapshot(&e.segment_id)?;
        }
        report.segments_downloaded += fetcher.segments;
        report.packs_downloaded += fetcher.packs;
        report.bytes_downloaded += fetcher.bytes;
    }
    fetch_org_files(svc, &g, &mut report)?;
    localize_roots(svc, &mut g);
    install_generation(svc, &g)?;
    tracing::info!(org = %svc.org, generation = g.generation, "pulled generation");
    Ok(Some(report))
}

/// Downloads the organization files `g` names that the mirror lacks (or
/// holds with another size).
fn fetch_org_files(svc: &ServiceStore, g: &OrgGeneration, report: &mut PullGenerationReport) -> anyhow::Result<()> {
    let storage = svc.storage.as_ref();
    let prefix = svc.org_path("");
    let listed: HashMap<String, u64> = storage.list(&prefix)?.into_iter().map(|m| (m.path, m.size)).collect();
    let index_prefix = format!("{prefix}index/");
    let index_files: BTreeSet<String> =
        listed.keys().filter_map(|p| p.strip_prefix(&index_prefix)).map(str::to_string).collect();
    let local = org_dir(&svc.local.dir, &svc.org);
    for (sub, file) in generation_files(g, &index_files) {
        let path = format!("{prefix}{sub}/{file}");
        let size = listed.get(&path).copied();
        let dest = local.join(sub).join(&file);
        if size.is_some_and(|s| fs::metadata(&dest).is_ok_and(|m| m.len() == s)) {
            continue;
        }
        fs::create_dir_all(local.join(sub))?;
        storage.get_to_file(&path, &dest).with_context(|| format!("downloading {path}"))?;
        report.files_downloaded += 1;
        report.bytes_downloaded += fs::metadata(&dest).map_or(0, |m| m.len());
    }
    Ok(())
}

/// Rewrites every entry's root to the mirror's synthetic root.
fn localize_roots(svc: &ServiceStore, g: &mut OrgGeneration) {
    for r in &mut g.repos {
        r.default.root = svc.mirror_root(&r.repo, None).to_string_lossy().into_owned();
        for x in &mut r.refs {
            x.entry.root = svc.mirror_root(&r.repo, Some(&x.ref_name)).to_string_lossy().into_owned();
        }
    }
}

/// Writes the generation file, then `org.json`, under the store lock.
fn install_generation(svc: &ServiceStore, g: &OrgGeneration) -> anyhow::Result<()> {
    let dir = org_dir(&svc.local.dir, &svc.org);
    let tmp = svc.local.dir.join("tmp");
    let bytes = serde_json::to_vec_pretty(g)?;
    let ptr = serde_json::to_vec_pretty(&OrgPointer { name: svc.org.clone(), generation: g.generation })?;
    let _lock = svc.local.lock()?;
    write_atomic(&dir.join(generation_file(g.generation)), &tmp, &bytes)?;
    write_atomic(&dir.join(POINTER_FILE), &tmp, &ptr)?;
    Ok(())
}
