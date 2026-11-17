//! Pulling snapshots into the mirror.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use rueca_segment::Segment;
use serde::Serialize;

use crate::catalog::{RefEntry, RefKind, RepoCatalog};
use crate::{ServiceStore, repo_dir};

const PACK_MAGIC: &[u8; 8] = b"RUECAPAK";
const PACK_END_MAGIC: &[u8; 8] = b"RCPAKEND";

/// Which refs of a repository [`pull_repo`] pulls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullWhich {
    /// The catalog's `default_ref`.
    Default,
    /// One ref by name.
    Ref(String),
    /// Every ref of the catalog.
    All,
}

/// One ref pulled by [`pull_repo`]: its head snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PulledRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub kind: RefKind,
    pub commit: String,
    pub segment: String,
    /// The synthetic root it is registered under.
    pub root: String,
    /// Whether the local catalog changed (a new head).
    pub registered: bool,
}

/// What [`pull_repo`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PullReport {
    pub repo: String,
    pub repo_dir: String,
    pub refs: Vec<PulledRef>,
    /// With [`PullWhich::All`]: refs of the mirror that the catalog no
    /// longer has (expired or removed), unregistered.
    pub unregistered: Vec<String>,
    pub segments_downloaded: usize,
    pub packs_downloaded: usize,
    pub bytes_downloaded: u64,
}

/// Pulls the head snapshot of the refs `which` selects of repository
/// `repo` into the mirror: downloads the segments and packs it does not
/// hold (see the crate docs, *Sequences*) and registers each head in the
/// local `catalog.json` under its synthetic root
/// ([`ServiceStore::mirror_root`]): the default ref as a working-tree
/// entry, other refs as ref snapshots. With [`PullWhich::All`], ref
/// entries of the repository the catalog no longer has are unregistered.
pub fn pull_repo(svc: &ServiceStore, repo: &str, which: PullWhich) -> anyhow::Result<PullReport> {
    let rd = repo_dir(repo, None);
    let path = ServiceStore::repo_path(&rd, "catalog.json");
    let Some(v) = svc.storage.get(&path)? else {
        bail!("repository `{repo}` is not published in the store (no {path})");
    };
    let catalog: RepoCatalog = serde_json::from_slice(&v.value).with_context(|| format!("{path} is not valid"))?;
    let mut fetcher = Fetcher::new(svc, &rd)?;
    let mut report = PullReport { repo: repo.to_string(), repo_dir: rd.clone(), ..PullReport::default() };
    for (name, entry) in select_refs(&catalog, &which)? {
        let Some(head) = entry.snapshots.first() else { continue };
        fetcher.snapshot(&head.segment)?;
        let as_ref = (catalog.default_ref.as_deref() != Some(name)).then_some(name);
        let root = svc.mirror_root(repo, as_ref);
        let registered = register(svc, &root, &head.segment, as_ref)?;
        report.refs.push(PulledRef {
            ref_name: name.to_string(),
            kind: entry.kind,
            commit: head.commit.clone(),
            segment: head.segment.clone(),
            root: root.to_string_lossy().into_owned(),
            registered,
        });
    }
    if which == PullWhich::All {
        report.unregistered = drop_unpublished(svc, repo, &catalog)?;
    }
    report.segments_downloaded = fetcher.segments;
    report.packs_downloaded = fetcher.packs;
    report.bytes_downloaded = fetcher.bytes;
    Ok(report)
}

fn select_refs<'c>(catalog: &'c RepoCatalog, which: &PullWhich) -> anyhow::Result<Vec<(&'c str, &'c RefEntry)>> {
    let one = |name: &str| -> anyhow::Result<Vec<(&'c str, &'c RefEntry)>> {
        match catalog.refs.get_key_value(name) {
            Some((k, e)) => Ok(vec![(k.as_str(), e)]),
            None => {
                let known: Vec<&str> = catalog.refs.keys().map(String::as_str).collect();
                bail!("ref `{name}` of `{}` is not published; published: {}", catalog.repo, known.join(", "))
            }
        }
    };
    match which {
        PullWhich::All => Ok(catalog.refs.iter().map(|(k, e)| (k.as_str(), e)).collect()),
        PullWhich::Ref(name) => one(name),
        PullWhich::Default => match &catalog.default_ref {
            Some(name) => one(name),
            None => bail!("repository `{}` has no default ref published", catalog.repo),
        },
    }
}

/// Unregisters the mirror's ref entries of `repo` (synthetic roots) whose
/// ref the catalog no longer has; returns their ref names.
fn drop_unpublished(svc: &ServiceStore, repo: &str, catalog: &RepoCatalog) -> anyhow::Result<Vec<String>> {
    let refs_dir = rueca_index::refs::repo_export_dir(&svc.local.dir, repo, "");
    let mut dropped = Vec::new();
    for e in svc.local.entries()? {
        let Some(name) = e.ref_name.as_deref() else { continue };
        let gone = !catalog.refs.contains_key(name) || catalog.default_ref.as_deref() == Some(name);
        if e.repo == repo && gone && Path::new(&e.root).starts_with(&refs_dir) {
            svc.local.unregister(&e.root)?;
            dropped.push(name.to_string());
        }
    }
    Ok(dropped)
}

/// Registers local segment `segment` under `root` unless the catalog
/// already has exactly that entry. Returns whether it changed.
fn register(svc: &ServiceStore, root: &Path, segment: &str, ref_name: Option<&str>) -> anyhow::Result<bool> {
    let root_s = root.to_string_lossy();
    let current = svc.local.entries()?.into_iter().find(|e| e.root == root_s);
    if current.is_some_and(|e| e.segment_id == segment && e.ref_name.as_deref() == ref_name) {
        return Ok(false);
    }
    let meta = svc.local.open_segment(segment)?.meta().clone();
    match ref_name {
        None => svc.local.register(&root_s, &meta)?,
        Some(r) => svc.local.register_ref(&root_s, &meta, r)?,
    }
    Ok(true)
}

/// Downloads the segments and packs of one repository directory into the
/// mirror, skipping what it already holds.
pub(crate) struct Fetcher<'a> {
    svc: &'a ServiceStore,
    rd: String,
    /// Sizes of the directory's objects, listed once.
    sizes: HashMap<String, u64>,
    pub segments: usize,
    pub packs: usize,
    pub bytes: u64,
}

impl<'a> Fetcher<'a> {
    pub fn new(svc: &'a ServiceStore, rd: &str) -> anyhow::Result<Fetcher<'a>> {
        let sizes = svc.storage.list(&ServiceStore::repo_path(rd, ""))?.into_iter().map(|m| (m.path, m.size)).collect();
        Ok(Fetcher { svc, rd: rd.to_string(), sizes, segments: 0, packs: 0, bytes: 0 })
    }

    /// Ensures segment `id` and its packs are in the mirror. The segment is
    /// downloaded to `tmp/`, validated by opening it, its packs fetched,
    /// and only then renamed into `segments/`.
    pub fn snapshot(&mut self, id: &str) -> anyhow::Result<()> {
        let dest = self.svc.local.segment_path(id);
        if dest.exists() {
            return Ok(());
        }
        let remote = ServiceStore::repo_path(&self.rd, &format!("segments/{id}.rcseg"));
        let tmp = self.download(&remote)?;
        let packs = Segment::open(tmp.path(), &self.svc.local.dir)
            .with_context(|| format!("validating {remote}"))?
            .meta()
            .packs
            .clone();
        for pack in &packs {
            self.pack(pack)?;
        }
        tmp.persist(&dest)?;
        self.segments += 1;
        Ok(())
    }

    /// Ensures pack `id` is in the mirror (an existing file must have the
    /// store's size).
    fn pack(&mut self, id: &str) -> anyhow::Result<()> {
        let dest = self.svc.local.dir.join("packs").join(format!("{id}.rcpack"));
        let remote = ServiceStore::repo_path(&self.rd, &format!("packs/{id}.rcpack"));
        let size = self.sizes.get(&remote).copied();
        if let Ok(m) = fs::metadata(&dest) {
            if size.is_none_or(|s| s == m.len()) {
                return Ok(());
            }
            bail!("local pack {} differs in size from {remote}", dest.display());
        }
        let tmp = self.download(&remote)?;
        check_pack(tmp.path(), size).with_context(|| format!("validating {remote}"))?;
        tmp.persist(&dest)?;
        self.packs += 1;
        Ok(())
    }

    /// Downloads `remote` into a scratch file of the mirror's `tmp/`.
    fn download(&mut self, remote: &str) -> anyhow::Result<Scratch> {
        let name = remote.rsplit('/').next().unwrap_or("object");
        let scratch = Scratch(self.svc.local.dir.join("tmp").join(format!(".pull-{:016x}-{name}", fastrand::u64(..))));
        self.svc.storage.get_to_file(remote, scratch.path()).with_context(|| format!("downloading {remote}"))?;
        let len = fs::metadata(scratch.path())?.len();
        if let Some(expected) = self.sizes.get(remote)
            && *expected != len
        {
            bail!("{remote}: downloaded {len} bytes, the store lists {expected}");
        }
        self.bytes += len;
        Ok(scratch)
    }
}

/// Checks a pack's header and trailer magic and, when known, its size.
fn check_pack(path: &Path, size: Option<u64>) -> anyhow::Result<()> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if size.is_some_and(|s| s != len) || len < 16 + 48 {
        bail!("pack size {len} is wrong");
    }
    let mut head = [0u8; 8];
    f.read_exact(&mut head)?;
    let mut tail = [0u8; 8];
    f.seek(SeekFrom::End(-8))?;
    f.read_exact(&mut tail)?;
    if &head != PACK_MAGIC || &tail != PACK_END_MAGIC {
        bail!("not a content pack (bad magic)");
    }
    Ok(())
}

/// A downloaded file in `tmp/`, removed on drop unless persisted.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Renames the file to `dest`.
    pub fn persist(self, dest: &Path) -> anyhow::Result<()> {
        fs::rename(&self.0, dest).with_context(|| format!("installing {}", dest.display()))?;
        Ok(())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
