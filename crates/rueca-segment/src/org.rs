//! The organization symbol index (RFC 0006 §3.2, §3.4, §5).
//!
//! An organization is a set of indexed roots (one repository each) that is
//! searched as one: **one tantivy index whose segments are the repositories'
//! own symbol-index segments**. Every segment written by
//! [`crate::write_segment`] embeds a tantivy index of exactly one tantivy
//! segment with doc id = node id; the organization index lists those
//! segments side by side, unchanged (same schema, tokenizer and priors — no
//! re-tokenization, no fork of the document format). tantivy computes BM25
//! statistics over every segment of an index, so scores are comparable
//! across repositories by construction.
//!
//! # Layout
//!
//! ```text
//! <store>/orgs/<name>/org.json                       pointer: {"name", "generation"}
//! <store>/orgs/<name>/generations/<generation>.json  OrgGeneration (immutable)
//! <store>/orgs/<name>/index/<uuid>.<ext>             tantivy segment files (immutable)
//! ```
//!
//! A generation lists the repositories (id, root, snapshot, the store
//! segment it was built from and its tantivy segment) plus the tantivy index
//! settings and schema. The index has no `meta.json` on disk: [`OrgIndex`]
//! serves one built from the generation through a read-only directory, so a
//! generation is a single small file and the segment files are shared
//! between generations.
//!
//! # Updates and generations
//!
//! [`update_org`] takes the organization's members (roots) and compares the
//! store catalog with the current generation. A repository whose segment is
//! unchanged keeps its entry; a new or re-indexed one has its tantivy
//! segment files copied out of its bundle into `index/` (the "delete by
//! repository, add the new documents" of RFC 0006 §3.2: segments are aligned
//! to repositories, so dropping a repository's documents is dropping its
//! segment from the list); a member no longer configured is dropped. Then
//! the new generation file is written and **`org.json` is swapped
//! atomically**. A reader reads the pointer once and keeps that generation
//! for the whole query. The current and the previous generation are kept;
//! older generation files, segment files no generation references and store
//! segments only those generations referenced are deleted (best effort).
//! [`Store::register`] never deletes a store segment a kept generation
//! references.
//!
//! # Queries, scope and visibility
//!
//! [`OrgIndex::search`] takes the **visible** repositories (permissions,
//! RFC 0006 §5) and the **selected** ones (the `repo` argument). Both are
//! applied inside tantivy: BM25 statistics are summed over the visible
//! segments only, and the collector skips every unselected segment before
//! scoring, so neither results, totals nor scores depend on a repository the
//! caller cannot read. Results are (repository, node) pairs ordered by
//! (score desc, repository order, node id). Priors and path globs come from
//! the repositories' opened [`Segment`]s.
//!
//! Single writer per store, as for the rest of the store.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rueca_model::{NodeId, SnapshotInfo};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    DirectoryLock, FileHandle, Lock, MmapDirectory, OwnedBytes, WatchCallback, WatchHandle, WritePtr,
};
use tantivy::index::SegmentId;
use tantivy::{Directory, Index, IndexReader, ReloadPolicy};

use crate::bundle::Bundle;
use crate::store::{atomic_write, canonical, ensure_dirs};
use crate::symbols::{self, PriorsBySegment, SubsetStats, TANTIVY_PREFIX};
use crate::tokenizer::{CODE_TOKENIZER, CodeTokenizer};
use crate::{FORMAT_VERSION, Result, Segment, SegmentError, SegmentMeta, Store, SymbolQuery};

/// Version of the generation file format.
pub const ORG_FORMAT: u32 = 1;

const ORGS_DIR: &str = "orgs";
const POINTER_FILE: &str = "org.json";
const GENERATIONS_DIR: &str = "generations";
const INDEX_DIR: &str = "index";
const META_FILE: &str = "meta.json";

/// One repository of a generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgRepo {
    /// Repository id (`github.com/acme/billing`, `local/billing`).
    pub repo: String,
    /// Canonical indexed root.
    pub root: String,
    /// Remote the root was cloned from, when the organization names a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Store segment the repository's documents come from.
    pub segment_id: String,
    pub snapshot: SnapshotInfo,
    /// tantivy segment (uuid, simple form) holding the repository's symbol
    /// documents; `None` for an empty graph.
    pub tantivy_segment: Option<String>,
    /// Symbol documents (= nodes of the segment).
    pub docs: u32,
    /// Bytes of the tantivy segment files.
    pub index_bytes: u64,
    /// Generation in which this entry was (re)built.
    pub since_generation: u64,
}

/// An immutable generation of an organization (`generations/<n>.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgGeneration {
    pub format: u32,
    pub name: String,
    pub generation: u64,
    /// Unix seconds.
    pub created_at: u64,
    /// Sorted by repository id; ids are unique.
    pub repos: Vec<OrgRepo>,
    /// tantivy `meta.json` without its segments: index settings and schema.
    pub tantivy: Value,
}

impl OrgGeneration {
    /// Index of the repository with id `repo`.
    pub fn repo_index(&self, repo: &str) -> Option<usize> {
        self.repos.iter().position(|r| r.repo == repo)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct OrgPointer {
    name: String,
    generation: u64,
}

/// A configured member of an organization: an indexed root.
#[derive(Debug, Clone, PartialEq)]
pub struct OrgMember {
    pub root: String,
    pub url: Option<String>,
}

/// What [`update_org`] did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OrgUpdate {
    pub name: String,
    /// The current generation after the update (unchanged when nothing changed).
    pub generation: u64,
    /// Whether a new generation was published.
    pub published: bool,
    pub repos: usize,
    /// Repository ids added, replaced (re-indexed) and removed.
    pub added: Vec<String>,
    pub replaced: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    /// Member roots that have no segment in the store yet (left out).
    pub missing: Vec<String>,
    /// Bytes of tantivy segment files copied into the organization index.
    pub bytes_copied: u64,
    pub duration_ms: u64,
}

fn corrupt(msg: impl Into<String>) -> SegmentError {
    SegmentError::Corrupt(msg.into())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Organization names: ASCII letters, digits, `-`, `_` and `.`, not starting
/// with `.`, at most 128 bytes (a directory name under `orgs/`).
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(SegmentError::Other(format!(
            "invalid organization name `{name}` (ASCII letters, digits, `-`, `_` and `.`, not starting with `.`)"
        )))
    }
}

/// `<store>/orgs/<name>`.
pub fn org_dir(store: &Path, name: &str) -> PathBuf {
    store.join(ORGS_DIR).join(name)
}

fn generation_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(GENERATIONS_DIR).join(format!("{generation:010}.json"))
}

fn read_generation(path: &Path) -> io::Result<Result<OrgGeneration>> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice::<OrgGeneration>(&bytes)
        .map_err(|e| corrupt(format!("{}: {e}", path.display())))
        .and_then(|g| if g.format == ORG_FORMAT { Ok(g) } else { Err(SegmentError::Version(g.format)) }))
}

/// The current generation of organization `name`, or `None` when it was
/// never built. Reads the pointer, then the generation it names (retried if
/// a concurrent writer pruned it in between).
pub fn current_generation(store: &Path, name: &str) -> Result<Option<OrgGeneration>> {
    validate_name(name)?;
    let dir = org_dir(store, name);
    for _ in 0..3 {
        let ptr: OrgPointer = match fs::read(dir.join(POINTER_FILE)) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| corrupt(format!("{POINTER_FILE} of `{name}`: {e}")))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match read_generation(&generation_path(&dir, ptr.generation)) {
            Ok(g) => return g.map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(corrupt(format!("organization `{name}`: the current generation file is missing")))
}

/// The current generation of every organization in the store, by name.
pub fn list_orgs(store: &Path) -> Result<Vec<OrgGeneration>> {
    let rd = match fs::read_dir(store.join(ORGS_DIR)) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let Some(name) = ent.file_name().to_str().map(str::to_string) else { continue };
        if validate_name(&name).is_err() || !ent.path().is_dir() {
            continue;
        }
        if let Some(g) = current_generation(store, &name)? {
            out.push(g);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Store segment ids referenced by any kept generation of any organization:
/// [`Store::register`] does not delete them.
pub(crate) fn referenced_segments(store: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(orgs) = fs::read_dir(store.join(ORGS_DIR)) else { return out };
    for org in orgs.flatten() {
        let Ok(gens) = fs::read_dir(org.path().join(GENERATIONS_DIR)) else { continue };
        for g in gens.flatten() {
            if let Ok(Ok(g)) = read_generation(&g.path()) {
                out.extend(g.repos.into_iter().map(|r| r.segment_id));
            }
        }
    }
    out
}

/// tantivy `meta.json` of an empty symbol index built by this code: the
/// index settings and schema every generation carries (segments removed).
fn tantivy_template() -> Result<Value> {
    let index = Index::create_in_ram(symbols::schema());
    let metas = index.load_metas().map_err(|e| SegmentError::Other(format!("tantivy: {e}")))?;
    let mut v = serde_json::to_value(&metas).map_err(|e| SegmentError::Other(e.to_string()))?;
    if let Value::Object(m) = &mut v {
        m.insert("segments".into(), Value::Array(Vec::new()));
        m.insert("opstamp".into(), json!(0));
        m.remove("payload");
    }
    Ok(v)
}

/// Builds or updates organization `name` from its `members` (see the module
/// docs): unchanged repositories are kept, new and re-indexed ones are
/// (re)added, others removed, and a new generation is published with an
/// atomic pointer swap. Nothing is published when nothing changed. Members
/// without a segment in the store are reported in `missing` and left out.
/// Two members with the same repository id are an error.
pub fn update_org(store: &Store, name: &str, members: &[OrgMember]) -> Result<OrgUpdate> {
    validate_name(name)?;
    let started = Instant::now();
    ensure_dirs(&store.dir)?;
    let dir = org_dir(&store.dir, name);
    fs::create_dir_all(dir.join(GENERATIONS_DIR))?;
    fs::create_dir_all(dir.join(INDEX_DIR))?;
    let tmp = store.dir.join("tmp");
    let prev = current_generation(&store.dir, name)?;
    let catalog = store.entries()?;
    let template = tantivy_template()?;
    let next = prev.as_ref().map_or(1, |g| g.generation + 1);
    let mut update = OrgUpdate { name: name.to_string(), ..OrgUpdate::default() };
    let mut repos: Vec<OrgRepo> = Vec::new();
    let mut roots = HashSet::new();
    for m in members {
        let root = canonical(Path::new(&m.root)).to_string_lossy().into_owned();
        if !roots.insert(root.clone()) {
            continue;
        }
        let Some(entry) = catalog.iter().find(|e| e.root == root) else {
            update.missing.push(root);
            continue;
        };
        let old = prev.as_ref().and_then(|g| g.repos.iter().find(|r| r.root == root));
        if let Some(old) = old
            && old.segment_id == entry.segment_id
            && prev.as_ref().is_some_and(|g| g.tantivy == template)
        {
            let mut keep = old.clone();
            if m.url.is_some() {
                keep.url = m.url.clone();
            }
            repos.push(keep);
            update.unchanged += 1;
            continue;
        }
        let repo = extract_repo(store, &dir, &tmp, &entry.root, &entry.segment_id, m.url.clone(), next, &template)?;
        update.bytes_copied += repo.index_bytes;
        if old.is_some() {
            update.replaced.push(repo.repo.clone());
        } else {
            update.added.push(repo.repo.clone());
        }
        repos.push(repo);
    }
    if let Some(g) = &prev {
        update.removed = g.repos.iter().filter(|r| !roots.contains(&r.root)).map(|r| r.repo.clone()).collect();
        // A member whose root is configured but no longer in the catalog.
        for r in &g.repos {
            if roots.contains(&r.root) && !repos.iter().any(|x| x.root == r.root) && !update.removed.contains(&r.repo) {
                update.removed.push(r.repo.clone());
            }
        }
    }
    repos.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.root.cmp(&b.root)));
    if let Some(w) = repos.windows(2).find(|w| w[0].repo == w[1].repo) {
        return Err(SegmentError::Other(format!(
            "repository id `{}` is indexed at two roots of organization `{name}` ({} and {}); give one of them \
             another id (`id = \"…\"` in the organization file, or `rueca index --repo`)",
            w[0].repo, w[0].root, w[1].root
        )));
    }
    update.repos = repos.len();
    let changed = match &prev {
        None => true,
        Some(g) => g.repos != repos || g.tantivy != template,
    };
    if !changed {
        update.generation = prev.map_or(0, |g| g.generation);
        update.duration_ms = started.elapsed().as_millis() as u64;
        return Ok(update);
    }
    let generation = OrgGeneration {
        format: ORG_FORMAT,
        name: name.to_string(),
        generation: next,
        created_at: now(),
        repos,
        tantivy: template,
    };
    let bytes = serde_json::to_vec_pretty(&generation).map_err(|e| SegmentError::Other(e.to_string()))?;
    atomic_write(&generation_path(&dir, next), &tmp, &bytes)?;
    let ptr = serde_json::to_vec_pretty(&OrgPointer { name: name.to_string(), generation: next })
        .map_err(|e| SegmentError::Other(e.to_string()))?;
    atomic_write(&dir.join(POINTER_FILE), &tmp, &ptr)?;
    collect_garbage(store, &dir, next);
    update.generation = next;
    update.published = true;
    update.duration_ms = started.elapsed().as_millis() as u64;
    Ok(update)
}

/// Copies the tantivy segment of store segment `segment_id` into the
/// organization index and describes it.
#[allow(clippy::too_many_arguments)]
fn extract_repo(
    store: &Store,
    dir: &Path,
    tmp: &Path,
    root: &str,
    segment_id: &str,
    url: Option<String>,
    generation: u64,
    template: &Value,
) -> Result<OrgRepo> {
    let bundle = Bundle::open(&store.segment_path(segment_id), FORMAT_VERSION)?;
    let meta: SegmentMeta =
        serde_json::from_slice(bundle.bytes("meta")?).map_err(|e| corrupt(format!("meta of {segment_id}: {e}")))?;
    let tmeta: Value = serde_json::from_slice(bundle.bytes(&format!("{TANTIVY_PREFIX}{META_FILE}"))?)
        .map_err(|e| corrupt(format!("symbol index meta of {segment_id}: {e}")))?;
    if tmeta.get("schema") != template.get("schema") {
        return Err(SegmentError::Other(format!(
            "the symbol index of {root} (segment {segment_id}) has another schema than this build; re-index it with \
             `rueca index`"
        )));
    }
    let segs = tmeta.get("segments").and_then(Value::as_array).ok_or_else(|| corrupt("symbol index meta segments"))?;
    let (tantivy_segment, docs, index_bytes) = match segs.as_slice() {
        [] => (None, 0, 0),
        [s] => {
            let sid: SegmentId = serde_json::from_value(s.get("segment_id").cloned().unwrap_or(Value::Null))
                .map_err(|e| corrupt(format!("symbol index segment id of {segment_id}: {e}")))?;
            let docs = s.get("max_doc").and_then(Value::as_u64).ok_or_else(|| corrupt("symbol index max_doc"))?;
            if s.get("deletes").is_some_and(|d| !d.is_null()) || docs != u64::from(meta.node_count) {
                return Err(corrupt(format!("symbol index of {segment_id} does not match its node table")));
            }
            let uuid = sid.uuid_string();
            let prefix = format!("{TANTIVY_PREFIX}{uuid}.");
            let mut bytes = 0u64;
            let index_dir = dir.join(INDEX_DIR);
            for (name, range) in &bundle.components {
                let Some(file) = name.strip_prefix(TANTIVY_PREFIX) else { continue };
                if !name.starts_with(&prefix) || file.contains('/') {
                    continue;
                }
                let data = &bundle.map[range.clone()];
                bytes += data.len() as u64;
                let dest = index_dir.join(file);
                if fs::metadata(&dest).is_ok_and(|m| m.len() == data.len() as u64) {
                    continue;
                }
                atomic_write(&dest, tmp, data)?;
            }
            (Some(uuid), docs as u32, bytes)
        }
        _ => return Err(corrupt(format!("symbol index of {segment_id} has {} tantivy segments", segs.len()))),
    };
    Ok(OrgRepo {
        repo: meta.snapshot.repo.clone(),
        root: root.to_string(),
        url,
        segment_id: segment_id.to_string(),
        snapshot: meta.snapshot,
        tantivy_segment,
        docs,
        index_bytes,
        since_generation: generation,
    })
}

/// Keeps generations `current` and `current - 1`; deletes older generation
/// files, segment files no kept generation references, and store segments
/// that only dropped generations referenced. Best effort.
fn collect_garbage(store: &Store, dir: &Path, current: u64) {
    let Ok(rd) = fs::read_dir(dir.join(GENERATIONS_DIR)) else { return };
    let mut kept: Vec<OrgGeneration> = Vec::new();
    let mut dropped: Vec<OrgGeneration> = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        let Some(n) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok()) else { continue };
        let Ok(Ok(g)) = read_generation(&path) else { continue };
        if n + 1 < current {
            let _ = fs::remove_file(&path);
            dropped.push(g);
        } else {
            kept.push(g);
        }
    }
    let live: HashSet<&str> =
        kept.iter().flat_map(|g| g.repos.iter().filter_map(|r| r.tantivy_segment.as_deref())).collect();
    if let Ok(rd) = fs::read_dir(dir.join(INDEX_DIR)) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or_default();
            if !live.contains(stem) {
                let _ = fs::remove_file(ent.path());
            }
        }
    }
    let catalog: HashSet<String> = store.entries().unwrap_or_default().into_iter().map(|e| e.segment_id).collect();
    let pinned = referenced_segments(&store.dir);
    for g in dropped {
        for r in g.repos {
            if !catalog.contains(&r.segment_id) && !pinned.contains(&r.segment_id) {
                let _ = fs::remove_file(store.segment_path(&r.segment_id));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read-only tantivy directory of one generation: `meta.json` built from the
/// generation, every other file from `index/`.
#[derive(Clone)]
struct OrgDirectory {
    meta: Arc<Vec<u8>>,
    files: MmapDirectory,
}

impl fmt::Debug for OrgDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OrgDirectory({:?})", self.files)
    }
}

fn read_only(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("organization index is read-only: {}", path.display()))
}

impl Directory for OrgDirectory {
    fn get_file_handle(&self, path: &Path) -> std::result::Result<Arc<dyn FileHandle>, OpenReadError> {
        if path == Path::new(META_FILE) {
            return Ok(Arc::new(OwnedBytes::new(self.meta.as_ref().clone())));
        }
        self.files.get_file_handle(path)
    }
    fn delete(&self, path: &Path) -> std::result::Result<(), DeleteError> {
        Err(DeleteError::IoError { io_error: Arc::new(read_only(path)), filepath: path.to_path_buf() })
    }
    fn exists(&self, path: &Path) -> std::result::Result<bool, OpenReadError> {
        if path == Path::new(META_FILE) {
            return Ok(true);
        }
        self.files.exists(path)
    }
    fn open_write(&self, path: &Path) -> std::result::Result<WritePtr, OpenWriteError> {
        Err(OpenWriteError::wrap_io_error(read_only(path), path.to_path_buf()))
    }
    fn atomic_read(&self, path: &Path) -> std::result::Result<Vec<u8>, OpenReadError> {
        if path == Path::new(META_FILE) {
            return Ok(self.meta.as_ref().clone());
        }
        self.files.atomic_read(path)
    }
    fn atomic_write(&self, path: &Path, _data: &[u8]) -> io::Result<()> {
        Err(read_only(path))
    }
    fn sync_directory(&self) -> io::Result<()> {
        Ok(())
    }
    fn acquire_lock(&self, _lock: &Lock) -> std::result::Result<DirectoryLock, LockError> {
        Ok(DirectoryLock::from(Box::new(())))
    }
    fn watch(&self, _cb: WatchCallback) -> tantivy::Result<WatchHandle> {
        Ok(WatchHandle::empty())
    }
}

/// What a query may read and what it selects (indexes into
/// [`OrgGeneration::repos`]).
#[derive(Clone, Copy)]
pub struct OrgScope<'a> {
    /// Repositories the caller may read: BM25 statistics and results come
    /// from these only.
    pub visible: &'a [usize],
    /// Repositories searched (the `repo` argument), with their opened
    /// segments (priors, path globs). Repositories outside `visible` are
    /// ignored.
    pub selected: &'a [(usize, &'a Segment)],
}

/// One result of [`OrgIndex::search`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrgHit {
    /// Index into [`OrgGeneration::repos`].
    pub repo: usize,
    pub node: NodeId,
    pub score: f32,
}

/// One generation of an organization's symbol index, opened for reading.
/// `Send + Sync`; a generation never changes, so it can be cached by
/// generation number.
pub struct OrgIndex {
    generation: OrgGeneration,
    reader: IndexReader,
    fields: symbols::Fields,
    /// tantivy segment of each repository (`None`: empty graph).
    segments: Vec<Option<SegmentId>>,
}

impl OrgIndex {
    /// Opens the current generation of organization `name`.
    pub fn open(store: &Path, name: &str) -> Result<OrgIndex> {
        let generation = current_generation(store, name)?.ok_or_else(|| {
            SegmentError::NotFound(format!(
                "organization `{name}` is not built in store `{}`; run `rueca index --org <file>`",
                store.display()
            ))
        })?;
        OrgIndex::open_generation(store, generation)
    }

    /// Opens `generation` (its segment files must still be in the store).
    pub fn open_generation(store: &Path, generation: OrgGeneration) -> Result<OrgIndex> {
        let dir = org_dir(store, &generation.name);
        let mut segments = Vec::with_capacity(generation.repos.len());
        let mut listed = Vec::new();
        for r in &generation.repos {
            let sid = match &r.tantivy_segment {
                Some(u) => Some(
                    SegmentId::from_uuid_string(u)
                        .map_err(|e| corrupt(format!("generation {}: segment id {u}: {e}", generation.generation)))?,
                ),
                None => None,
            };
            if let Some(sid) = sid {
                listed.push(json!({"segment_id": sid, "max_doc": r.docs, "deletes": null}));
            }
            segments.push(sid);
        }
        let mut meta = generation.tantivy.clone();
        let Value::Object(m) = &mut meta else {
            return Err(corrupt(format!("generation {} has no index settings", generation.generation)));
        };
        m.insert("segments".into(), Value::Array(listed));
        let meta = serde_json::to_vec(&meta).map_err(|e| SegmentError::Other(e.to_string()))?;
        let files = MmapDirectory::open(dir.join(INDEX_DIR))
            .map_err(|e| SegmentError::Other(format!("organization index {}: {e}", dir.display())))?;
        let index = Index::open(OrgDirectory { meta: Arc::new(meta), files })
            .map_err(|e| corrupt(format!("organization `{}` index: {e}", generation.name)))?;
        index.tokenizers().register(CODE_TOKENIZER, CodeTokenizer);
        let fields = symbols::fields(&index.schema())?;
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| corrupt(format!("organization `{}` index: {e}", generation.name)))?;
        let searcher = reader.searcher();
        for sr in searcher.segment_readers() {
            let i = segments
                .iter()
                .position(|s| *s == Some(sr.segment_id()))
                .ok_or_else(|| corrupt("organization index lists an unknown segment"))?;
            if sr.max_doc() != generation.repos[i].docs || sr.num_docs() != sr.max_doc() {
                return Err(corrupt(format!("organization index: documents of {} disagree", generation.repos[i].repo)));
            }
        }
        Ok(OrgIndex { generation, reader, fields, segments })
    }

    pub fn generation(&self) -> &OrgGeneration {
        &self.generation
    }

    /// BM25 search over the selected repositories with statistics over the
    /// visible ones (see the module docs). `limit == 0` means 20; `offset`
    /// pages the merged ranking. Ordered by score descending, then
    /// repository order, then node id.
    pub fn search(&self, query: &SymbolQuery, scope: &OrgScope<'_>) -> Result<Vec<OrgHit>> {
        let limit = symbols::effective_limit(query);
        let glob = query.path_glob.as_deref().map(symbols::compile_glob).transpose()?;
        let visible: HashSet<SegmentId> =
            scope.visible.iter().filter_map(|&i| self.segments.get(i).copied().flatten()).collect();
        let mut priors = PriorsBySegment::new();
        let mut by_segment: HashMap<SegmentId, (usize, &Segment)> = HashMap::new();
        for &(i, seg) in scope.selected {
            let repo =
                self.generation.repos.get(i).ok_or_else(|| SegmentError::NotFound(format!("repository #{i}")))?;
            if !scope.visible.contains(&i) {
                continue;
            }
            if seg.meta().id != repo.segment_id {
                return Err(SegmentError::Other(format!(
                    "segment {} is not the one organization `{}` was built from for {} ({})",
                    seg.meta().id,
                    self.generation.name,
                    repo.repo,
                    repo.segment_id
                )));
            }
            let Some(sid) = self.segments[i] else { continue };
            if seg.nodes().len() != repo.docs as usize {
                return Err(corrupt(format!("organization index: node table of {} disagrees", repo.repo)));
            }
            priors.insert(sid, seg.inner.symbols().priors());
            by_segment.insert(sid, (i, seg));
        }
        let searcher = self.reader.searcher();
        let stats = SubsetStats { searcher: &searcher, segments: &visible };
        let q = symbols::build_query(&self.fields, query);
        let keep = |sid: SegmentId, doc: u32| {
            by_segment.get(&sid).is_some_and(|(_, seg)| {
                (doc as usize) < seg.nodes().len() && glob.as_ref().is_none_or(|g| g.is_match(seg.inner.node_path(doc)))
            })
        };
        let want = query.offset.saturating_add(limit);
        let hits = symbols::top_hits(&searcher, q.as_ref(), &stats, Arc::new(priors), want, &keep)?;
        Ok(hits
            .into_iter()
            .skip(query.offset)
            .take(limit)
            .filter_map(|h| by_segment.get(&h.segment).map(|(i, _)| OrgHit { repo: *i, node: h.doc, score: h.score }))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["acme", "acme-corp", "a.b_c", "A1"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".hidden", "a/b", "a b", "../x", &"x".repeat(129)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn template_has_schema_and_no_segments() {
        let t = tantivy_template().unwrap();
        assert!(t["schema"].as_array().is_some_and(|a| !a.is_empty()));
        assert_eq!(t["segments"], json!([]));
        assert!(t.get("index_settings").is_some());
    }
}
