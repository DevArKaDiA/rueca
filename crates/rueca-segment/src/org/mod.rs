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
//! <store>/orgs/<name>/profiles/<store segment>.json   RepoProfile of one entry
//! <store>/orgs/<name>/links/<store segment>-<hash>.bin  link set of one consumer entry
//! ```
//!
//! A generation lists the repositories — per repository its entries (the
//! default and its refs: root, snapshot, the store segment each was built
//! from and its tantivy segment) — plus the tantivy index settings and
//! schema. The index has no `meta.json` on disk: [`OrgIndex`]
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
//! # Routing and the link layer (RFC 0006 phase 2)
//!
//! Every repository entry also names a **profile**
//! (`profiles/<store segment>.json`, [`RepoProfile`]): the repository's
//! routing document and its package registry entries. [`OrgIndex::route`]
//! scores the visible repositories for a query ([`route`]) and
//! [`OrgIndex::search_with`] weighs the symbol ranking with it. With an
//! [`OrgLinker`] ([`update_org_with`]), each repository also gets a **link
//! set** (`links/…`, [`link`]): its cross-repository edges into the
//! repositories publishing the packages it imports, recomputed only when
//! the repository, one of its dependencies or its dependency set changes.
//! [`OrgIndex::link_edges`] serves them.
//!
//! # Refs (RFC 0006 §9.3)
//!
//! A repository's [`OrgRepo`] describes its **default entry** (the member
//! root) and lists its **ref entries** ([`OrgRef`]: the ref snapshots of
//! the store that carry the repository's id, exported under
//! `<store>/refs/`). Every entry has the same per-snapshot fields
//! ([`OrgEntry`]: store and tantivy segment, profile, link set), and the
//! organization index lists the default **and** the ref segments. An
//! update reuses each entry by (root, store segment) on its own: a
//! refreshed ref replaces only its entry. A ref's link set points into its
//! dependencies' default entries, so changing a ref never re-links another
//! repository. Queries choose the entries they read per repository
//! ([`OrgSelection`], [`OrgSlot`]).
//!
//! Single writer per store, as for the rest of the store.

pub mod link;
pub mod route;
pub mod select;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rueca_model::{EdgeKind, NodeId, SnapshotInfo};
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
use crate::{CatalogEntry, FORMAT_VERSION, Result, Segment, SegmentError, SegmentMeta, Store, SymbolQuery};

pub use link::{
    ContractKey, LinkDir, LinkEdge, LinkInput, LinkRef, LinkTable, NoLinker, OrgDep, OrgLinker, OrgLinks, Package,
    RepoDeps,
};
pub use route::{PROFILE_FORMAT, RepoProfile, RepoRoute, RouteMode, RouteParams};
pub use select::{OrgRefInfo, OrgSelection, OrgSlot, ref_names_match, snapshot_ref_name};

/// Version of the generation file format.
pub const ORG_FORMAT: u32 = 1;

const ORGS_DIR: &str = "orgs";
const POINTER_FILE: &str = "org.json";
const GENERATIONS_DIR: &str = "generations";
const INDEX_DIR: &str = "index";
const PROFILES_DIR: &str = "profiles";
const META_FILE: &str = "meta.json";

/// One indexed snapshot of a repository in a generation: its default
/// entry (the member root) or one of its ref entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgEntry {
    /// Canonical indexed root (a ref entry: its export directory under
    /// `<store>/refs/`).
    pub root: String,
    /// Store segment the entry's documents come from.
    pub segment_id: String,
    pub snapshot: SnapshotInfo,
    /// tantivy segment (uuid, simple form) holding the entry's symbol
    /// documents; `None` for an empty graph.
    pub tantivy_segment: Option<String>,
    /// Symbol documents (= nodes of the segment).
    pub docs: u32,
    /// Bytes of the tantivy segment files.
    pub index_bytes: u64,
    /// Generation in which this entry was (re)built.
    pub since_generation: u64,
    /// Routing profile file under `profiles/` ([`RepoProfile`], one per
    /// store segment); absent in generations written before RFC 0006 phase 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Cross-repository edges of this entry into its dependencies' default
    /// entries; absent when it depends on no repository of the organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<OrgLinks>,
}

/// One repository of a generation: its default entry (flattened, so a
/// generation written before refs reads unchanged) and its ref entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgRepo {
    /// Repository id (`github.com/acme/billing`, `local/billing`).
    pub repo: String,
    /// Remote the root was cloned from, when the organization names a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The default ref: the member root (a working tree, or a `url`
    /// member's clone).
    #[serde(flatten)]
    pub default: OrgEntry,
    /// The other indexed refs, sorted by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<OrgRef>,
}

/// A ref entry of a repository: a ref snapshot (`rueca index --ref`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgRef {
    /// The ref name of the catalog entry (`feature/x`, `pr/7`, a 12-digit
    /// commit).
    pub ref_name: String,
    #[serde(flatten)]
    pub entry: OrgEntry,
}

impl OrgRepo {
    /// The default entry (`None`) or ref entry `k`.
    pub fn entry(&self, ref_idx: Option<usize>) -> Option<&OrgEntry> {
        match ref_idx {
            None => Some(&self.default),
            Some(k) => self.refs.get(k).map(|r| &r.entry),
        }
    }

    /// `(ref index, entry)` of every entry: the default first.
    pub fn slots(&self) -> impl Iterator<Item = (Option<usize>, &OrgEntry)> + '_ {
        std::iter::once((None, &self.default)).chain(self.refs.iter().enumerate().map(|(k, r)| (Some(k), &r.entry)))
    }

    /// Every entry: the default first.
    pub fn entries(&self) -> impl Iterator<Item = &OrgEntry> + '_ {
        self.slots().map(|(_, e)| e)
    }
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
    /// Version of the [`OrgLinker`] that produced profiles and links (0:
    /// none).
    #[serde(default)]
    pub linker: u32,
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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrgMember {
    pub root: String,
    pub url: Option<String>,
    /// Refs configured for the member (`refs = [...]` in the organization
    /// file): one not indexed is reported in [`OrgUpdate::missing`] as
    /// `repo@ref`. They do not filter: the generation lists every ref
    /// snapshot of the member's repository (see [`update_org_with`]).
    pub refs: Vec<String>,
}

/// Work done on one entry by an update, in microseconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OrgEntryCost {
    /// `repo` for a default entry, `repo@ref` for a ref entry.
    pub entry: String,
    /// Copying its tantivy segment into the organization index.
    pub copy_us: u64,
    pub bytes_copied: u64,
    /// Computing its profile.
    pub profile_us: u64,
    /// Computing its link set.
    pub link_us: u64,
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
    /// Repository ids added, replaced (re-indexed) and removed (default
    /// entries).
    pub added: Vec<String>,
    pub replaced: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    /// Ref entries in the published generation.
    pub refs: usize,
    /// Ref entries (`repo@ref`) added, replaced (re-indexed) and removed;
    /// ref entries kept as they were.
    pub refs_added: Vec<String>,
    pub refs_replaced: Vec<String>,
    pub refs_removed: Vec<String>,
    pub refs_unchanged: usize,
    /// Member roots that have no segment in the store yet (left out), and
    /// configured refs (`repo@ref`) not indexed.
    pub missing: Vec<String>,
    /// Ref snapshots just indexed for the organization whose repository id
    /// is no member's (left out), as text: filled by the indexer that
    /// indexed them (`rueca_index::org`), empty from [`update_org`] alone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unowned_refs: Vec<String>,
    /// Bytes of tantivy segment files copied into the organization index.
    pub bytes_copied: u64,
    /// Entries whose profile (routing document, packages) was computed
    /// (`repo` or `repo@ref`).
    pub profiled: Vec<String>,
    /// Entries whose link set was (re)computed, with their edge counts.
    pub linked: Vec<(String, u32)>,
    /// Link edges in the published generation.
    pub link_edges: u64,
    /// Entries whose linking failed (they have no links).
    pub link_errors: Vec<String>,
    /// Milliseconds spent on profiles and on links.
    pub profile_ms: u64,
    pub link_ms: u64,
    pub duration_ms: u64,
    /// Per entry that was copied, profiled or linked: what it cost.
    pub costs: Vec<OrgEntryCost>,
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
                out.extend(g.repos.iter().flat_map(OrgRepo::entries).map(|e| e.segment_id.clone()));
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
/// Two members with the same repository id are an error. Profiles are
/// computed without packages and no links are built ([`NoLinker`]); see
/// [`update_org_with`].
pub fn update_org(store: &Store, name: &str, members: &[OrgMember]) -> Result<OrgUpdate> {
    update_org_with(store, name, members, &NoLinker)
}

/// [`update_org`] with a linker: every new or re-indexed entry is analyzed
/// (packages, unresolved imports, required packages) into its profile, then
/// each entry whose dependency set, own segment or dependencies' segments
/// changed gets its link set recomputed (the reverse dependencies of a
/// repository whose default entry changed included). A failing link
/// computation is reported in `link_errors` and leaves that entry without
/// links; it never fails the update.
///
/// **Refs.** A member's ref entries are the catalog's ref snapshots
/// ([`CatalogEntry::ref_name`]) exported under `<store>/refs/` that carry
/// the repository id of the member's default entry: **an export belongs to
/// the member whose repository id it carries** (ids are unique within an
/// organization), whether the organization file lists it or it was indexed
/// from the command line. Each is reused by (root, store segment) like a
/// default entry: a re-indexed ref replaces only its own entry (segment
/// copy, profile, link set); an unregistered one is dropped. A ref entry's
/// link set is computed against its dependencies' default entries, from
/// its own imports; refs never trigger a reverse-dependency recompute.
pub fn update_org_with(store: &Store, name: &str, members: &[OrgMember], linker: &dyn OrgLinker) -> Result<OrgUpdate> {
    validate_name(name)?;
    let started = Instant::now();
    ensure_dirs(&store.dir)?;
    let dir = org_dir(&store.dir, name);
    for d in [GENERATIONS_DIR, INDEX_DIR, PROFILES_DIR, link::LINKS_DIR] {
        fs::create_dir_all(dir.join(d))?;
    }
    let tmp = store.dir.join("tmp");
    let prev = current_generation(&store.dir, name)?;
    let catalog = store.entries()?;
    let template = tantivy_template()?;
    let mut b = OrgBuild {
        store,
        linker,
        version: linker.version(),
        next: prev.as_ref().map_or(1, |g| g.generation + 1),
        dir,
        tmp,
        refs_dir: canonical(&store.dir).join(REFS_DIR),
        same_template: prev.as_ref().is_some_and(|g| g.tantivy == template),
        prev,
        template,
        update: OrgUpdate { name: name.to_string(), ..OrgUpdate::default() },
        segments: HashMap::new(),
        costs: BTreeMap::new(),
    };
    let (mut repos, roots) = b.collect_repos(members, &catalog)?;
    if let Some(g) = &b.prev {
        b.update.removed = removed_repos(g, &roots, &repos);
        b.update.refs_removed = removed_refs(g, &repos);
    }
    repos.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.default.root.cmp(&b.default.root)));
    if let Some(w) = repos.windows(2).find(|w| w[0].repo == w[1].repo) {
        return Err(SegmentError::Other(format!(
            "repository id `{}` is indexed at two roots of organization `{name}` ({} and {}); give one of them \
             another id (`id = \"…\"` in the organization file, or `rueca index --repo`)",
            w[0].repo, w[0].default.root, w[1].default.root
        )));
    }

    // Profiles: kept when present for the same segment, profile format and
    // linker version; computed otherwise.
    let t = Instant::now();
    let profiles = b.build_profiles(&mut repos)?;
    b.update.profile_ms = t.elapsed().as_millis() as u64;

    // Links: per consumer entry, the repositories publishing what it imports.
    let t = Instant::now();
    b.link_all(&mut repos, &profiles)?;
    b.update.link_ms = t.elapsed().as_millis() as u64;
    b.publish(name, repos, started)
}

/// `<store>/refs`: where ref snapshots are exported.
const REFS_DIR: &str = "refs";

/// The profiles of a generation being built: per repository, its default
/// entry's and its ref entries'.
struct Profiles {
    default: Vec<RepoProfile>,
    refs: Vec<Vec<RepoProfile>>,
}

impl Profiles {
    fn of(&self, slot: OrgSlot) -> &RepoProfile {
        match slot.ref_idx {
            None => &self.default[slot.repo],
            Some(k) => &self.refs[slot.repo][k],
        }
    }
}

/// `repo` or `repo@ref`: how updates name an entry.
fn entry_label(repo: &str, ref_name: Option<&str>) -> String {
    match ref_name {
        Some(r) => format!("{repo}@{r}"),
        None => repo.to_string(),
    }
}

fn micros(t: Instant) -> u64 {
    t.elapsed().as_micros() as u64
}

/// The state of one [`update_org_with`] run.
struct OrgBuild<'s> {
    store: &'s Store,
    linker: &'s dyn OrgLinker,
    /// The organization directory.
    dir: PathBuf,
    tmp: PathBuf,
    /// `<store>/refs`, canonical: ref exports live under it.
    refs_dir: PathBuf,
    prev: Option<OrgGeneration>,
    /// The previous generation has this build's index settings and schema.
    same_template: bool,
    template: Value,
    version: u32,
    /// The generation this run publishes.
    next: u64,
    update: OrgUpdate,
    /// Store segments opened so far, by id.
    segments: HashMap<String, Arc<Segment>>,
    /// What each touched entry cost, by label.
    costs: BTreeMap<String, OrgEntryCost>,
}

impl OrgBuild<'_> {
    /// One repository per distinct member root that has a segment, with
    /// its ref entries; the member roots.
    fn collect_repos(
        &mut self,
        members: &[OrgMember],
        catalog: &[CatalogEntry],
    ) -> Result<(Vec<OrgRepo>, HashSet<String>)> {
        let mut repos: Vec<OrgRepo> = Vec::new();
        let mut roots = HashSet::new();
        for m in members {
            let root = canonical(Path::new(&m.root)).to_string_lossy().into_owned();
            if !roots.insert(root.clone()) {
                continue;
            }
            match catalog.iter().find(|e| e.root == root) {
                Some(entry) => {
                    let mut repo = self.member_repo(m, entry)?;
                    repo.refs = self.member_refs(m, &repo.repo, catalog)?;
                    repos.push(repo);
                }
                None => self.update.missing.push(root),
            }
        }
        Ok((repos, roots))
    }

    /// The entry of the previous generation (default or ref) at `root`.
    fn prev_entry(&self, root: &str) -> Option<&OrgEntry> {
        self.prev.as_ref()?.repos.iter().flat_map(OrgRepo::entries).find(|e| e.root == root)
    }

    /// The entry at `root` of the previous generation when it is still
    /// valid: same store segment, same index settings.
    fn reusable_entry(&self, root: &str, segment_id: &str) -> Option<OrgEntry> {
        self.prev_entry(root).filter(|e| e.segment_id == segment_id && self.same_template).cloned()
    }

    /// The repository of a member (its default entry, no refs yet): the
    /// previous generation's when its segment and the index template are
    /// unchanged, else extracted again.
    fn member_repo(&mut self, m: &OrgMember, entry: &CatalogEntry) -> Result<OrgRepo> {
        let old = self.prev.as_ref().and_then(|g| g.repos.iter().find(|r| r.default.root == entry.root));
        if let Some(old) = old
            && let Some(keep) = self.reusable_entry(&entry.root, &entry.segment_id)
        {
            let url = m.url.clone().or_else(|| old.url.clone());
            let repo = old.repo.clone();
            self.update.unchanged += 1;
            return Ok(OrgRepo { repo, url, default: keep, refs: Vec::new() });
        }
        let replaced = old.is_some();
        let (repo, default) = self.extract(&entry.root, &entry.segment_id, None)?;
        if replaced {
            self.update.replaced.push(repo.clone());
        } else {
            self.update.added.push(repo.clone());
        }
        Ok(OrgRepo { repo, url: m.url.clone(), default, refs: Vec::new() })
    }

    /// Whether catalog entry `e` is a ref export of repository `repo`.
    fn owns(&self, repo: &str, e: &CatalogEntry) -> bool {
        e.repo == repo && e.is_ref_export() && Path::new(&e.root).starts_with(&self.refs_dir)
    }

    /// The ref entries of repository `repo`, sorted by name; configured refs
    /// of `m` that are not indexed go to `missing`.
    fn member_refs(&mut self, m: &OrgMember, repo: &str, catalog: &[CatalogEntry]) -> Result<Vec<OrgRef>> {
        let mut out = Vec::new();
        let mine: Vec<&CatalogEntry> = catalog.iter().filter(|e| self.owns(repo, e)).collect();
        for e in mine {
            out.push(self.ref_entry(repo, e)?);
        }
        out.sort_by(|a, b| a.ref_name.cmp(&b.ref_name).then(a.entry.root.cmp(&b.entry.root)));
        for want in &m.refs {
            if !out.iter().any(|r| ref_names_match(&r.ref_name, want)) {
                self.update.missing.push(entry_label(repo, Some(want)));
            }
        }
        Ok(out)
    }

    /// One ref entry: the previous generation's when still valid, else
    /// extracted again.
    fn ref_entry(&mut self, repo: &str, e: &CatalogEntry) -> Result<OrgRef> {
        let ref_name = e.ref_name.clone().unwrap_or_default();
        if let Some(entry) = self.reusable_entry(&e.root, &e.segment_id) {
            self.update.refs_unchanged += 1;
            return Ok(OrgRef { ref_name, entry });
        }
        let replaced = self.prev_entry(&e.root).is_some();
        let label = entry_label(repo, Some(&ref_name));
        let (_, entry) = self.extract(&e.root, &e.segment_id, Some(&ref_name))?;
        if replaced {
            self.update.refs_replaced.push(label);
        } else {
            self.update.refs_added.push(label);
        }
        Ok(OrgRef { ref_name, entry })
    }

    /// Copies an entry's tantivy segment into the organization index.
    fn extract(&mut self, root: &str, segment_id: &str, ref_name: Option<&str>) -> Result<(String, OrgEntry)> {
        let t = Instant::now();
        let (repo, entry) =
            extract_entry(self.store, &self.dir, &self.tmp, root, segment_id, self.next, &self.template)?;
        self.update.bytes_copied += entry.index_bytes;
        let cost = self.cost(&entry_label(&repo, ref_name));
        cost.copy_us += micros(t);
        cost.bytes_copied += entry.index_bytes;
        Ok((repo, entry))
    }

    fn cost(&mut self, label: &str) -> &mut OrgEntryCost {
        self.costs
            .entry(label.to_string())
            .or_insert_with(|| OrgEntryCost { entry: label.to_string(), ..Default::default() })
    }

    /// A store segment, opened once.
    fn open(&mut self, id: &str) -> Result<Arc<Segment>> {
        if let Some(s) = self.segments.get(id) {
            return Ok(s.clone());
        }
        let s = Arc::new(self.store.open_segment(id)?);
        self.segments.insert(id.to_string(), s.clone());
        Ok(s)
    }

    /// The profile of every entry, read back or computed.
    fn build_profiles(&mut self, repos: &mut [OrgRepo]) -> Result<Profiles> {
        let mut out = Profiles { default: Vec::with_capacity(repos.len()), refs: Vec::with_capacity(repos.len()) };
        for r in repos.iter_mut() {
            let p = self.entry_profile(&entry_label(&r.repo, None), &mut r.default)?;
            out.default.push(p);
            let mut refs = Vec::with_capacity(r.refs.len());
            for x in &mut r.refs {
                refs.push(self.entry_profile(&entry_label(&r.repo, Some(&x.ref_name)), &mut x.entry)?);
            }
            out.refs.push(refs);
        }
        Ok(out)
    }

    /// The profile of one entry (named `label`), read back or computed.
    fn entry_profile(&mut self, label: &str, e: &mut OrgEntry) -> Result<RepoProfile> {
        let file = profile_file_name(&e.segment_id);
        let current = read_profile(&self.dir, &file).filter(|p| p.format == PROFILE_FORMAT && p.linker == self.version);
        let profile = match current {
            Some(p) => p,
            None => self.compute_profile(label, e, &file)?,
        };
        e.profile = Some(file);
        Ok(profile)
    }

    /// Analyzes an entry with the linker and stores its profile.
    fn compute_profile(&mut self, label: &str, e: &OrgEntry, file: &str) -> Result<RepoProfile> {
        let t = Instant::now();
        let seg = self.open(&e.segment_id)?;
        let deps = match self.linker.analyze(self.store, &seg) {
            Ok(deps) => deps,
            Err(err) => {
                self.update.link_errors.push(format!("{label}: analysis: {err}"));
                RepoDeps::default()
            }
        };
        let p = RepoProfile::build(&seg, deps, self.version);
        let bytes = serde_json::to_vec(&p).map_err(|e| SegmentError::Other(e.to_string()))?;
        atomic_write(&self.dir.join(PROFILES_DIR).join(file), &self.tmp, &bytes)?;
        self.update.profiled.push(label.to_string());
        self.cost(label).profile_us += micros(t);
        Ok(p)
    }

    /// The link set of every entry: defaults against the repositories
    /// publishing what they import, refs likewise from their own imports
    /// (always against the dependencies' default entries).
    fn link_all(&mut self, repos: &mut [OrgRepo], profiles: &Profiles) -> Result<()> {
        for i in 0..repos.len() {
            for slot in std::iter::once(None).chain((0..repos[i].refs.len()).map(Some)) {
                let slot = OrgSlot { repo: i, ref_idx: slot };
                let want = wanted_deps(profiles.of(slot), i, &profiles.default, self.version);
                let links = self.links_of(slot, repos, profiles, &want)?;
                match slot.ref_idx {
                    None => repos[i].default.links = links,
                    Some(k) => repos[i].refs[k].entry.links = links,
                }
            }
        }
        Ok(())
    }

    /// The links of entry `slot` to the repositories `want` (none without
    /// dependencies): the previous ones when still valid, else computed.
    fn links_of(
        &mut self,
        slot: OrgSlot,
        repos: &[OrgRepo],
        profiles: &Profiles,
        want: &[usize],
    ) -> Result<Option<OrgLinks>> {
        let deps: Vec<OrgDep> = want
            .iter()
            .map(|&j| OrgDep { repo: repos[j].repo.clone(), segment_id: repos[j].default.segment_id.clone() })
            .collect();
        if deps.is_empty() {
            return Ok(None);
        }
        let entry = repos[slot.repo].entry(slot.ref_idx).ok_or_else(|| corrupt("entry out of range"))?;
        let links = match self.reusable_links(entry, &deps) {
            Some(l) => l,
            None => self.compute_links(slot, repos, profiles, want, deps)?,
        };
        self.update.link_edges += u64::from(links.edges);
        Ok(Some(links))
    }

    /// The previous generation's links of entry `e`, when its segment, the
    /// linker version and the dependencies are the same and the link file is
    /// there.
    fn reusable_links(&self, e: &OrgEntry, deps: &[OrgDep]) -> Option<OrgLinks> {
        let g = self.prev.as_ref()?;
        let x = self.prev_entry(&e.root)?;
        if x.segment_id != e.segment_id || g.linker != self.version {
            return None;
        }
        let l = x.links.clone()?;
        let file_ok = l.file.as_ref().is_none_or(|f| self.dir.join(link::LINKS_DIR).join(f).exists());
        (l.deps == deps && file_ok).then_some(l)
    }

    /// Runs the linker for consumer entry `slot` and stores its edges.
    fn compute_links(
        &mut self,
        slot: OrgSlot,
        repos: &[OrgRepo],
        profiles: &Profiles,
        want: &[usize],
        deps: Vec<OrgDep>,
    ) -> Result<OrgLinks> {
        let t = Instant::now();
        let r = &repos[slot.repo];
        let entry = r.entry(slot.ref_idx).ok_or_else(|| corrupt("entry out of range"))?;
        let label = entry_label(&r.repo, slot.ref_idx.map(|k| r.refs[k].ref_name.as_str()));
        let file = link::link_file_name(&entry.segment_id, &deps, self.version);
        let consumer_seg = self.open(&entry.segment_id)?;
        let dep_segs = want.iter().map(|&j| self.open(&repos[j].default.segment_id)).collect::<Result<Vec<_>>>()?;
        let consumer = LinkInput { repo: r, segment: &consumer_seg, deps: &profiles.of(slot).deps };
        let inputs: Vec<LinkInput<'_>> = want
            .iter()
            .zip(&dep_segs)
            .map(|(&j, s)| LinkInput { repo: &repos[j], segment: s, deps: &profiles.default[j].deps })
            .collect();
        let links = match self.linker.link(self.store, &consumer, &inputs) {
            Ok(edges) => {
                let n = edges.len() as u32;
                let file = if edges.is_empty() {
                    None
                } else {
                    link::write_links(&self.dir, &self.tmp, &file, &edges)?;
                    Some(file)
                };
                self.update.linked.push((label.clone(), n));
                OrgLinks { file, deps, edges: n, error: None }
            }
            Err(e) => {
                self.update.link_errors.push(format!("{label}: {e}"));
                OrgLinks { file: None, deps, edges: 0, error: Some(e.to_string()) }
            }
        };
        self.cost(&label).link_us += micros(t);
        Ok(links)
    }

    /// Publishes generation `next` when anything changed.
    fn publish(self, name: &str, repos: Vec<OrgRepo>, started: Instant) -> Result<OrgUpdate> {
        let OrgBuild { store, dir, tmp, prev, template, version, next, mut update, costs, .. } = self;
        update.repos = repos.len();
        update.refs = repos.iter().map(|r| r.refs.len()).sum();
        update.costs = costs.into_values().collect();
        let changed = match &prev {
            None => true,
            Some(g) => g.repos != repos || g.tantivy != template || g.linker != version,
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
            linker: version,
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
}

/// Repositories of the previous generation `g` that are gone: their root
/// is no longer a member, or no longer in the catalog.
fn removed_repos(g: &OrgGeneration, roots: &HashSet<String>, repos: &[OrgRepo]) -> Vec<String> {
    let mut removed: Vec<String> =
        g.repos.iter().filter(|r| !roots.contains(&r.default.root)).map(|r| r.repo.clone()).collect();
    // A member whose root is configured but no longer in the catalog.
    for r in &g.repos {
        let gone = !repos.iter().any(|x| x.default.root == r.default.root);
        if roots.contains(&r.default.root) && gone && !removed.contains(&r.repo) {
            removed.push(r.repo.clone());
        }
    }
    removed
}

/// Ref entries (`repo@ref`) of the previous generation `g` that are not in
/// `repos` any more (unregistered, or their repository left).
fn removed_refs(g: &OrgGeneration, repos: &[OrgRepo]) -> Vec<String> {
    let now: HashSet<&str> = repos.iter().flat_map(|r| r.refs.iter().map(|x| x.entry.root.as_str())).collect();
    g.repos
        .iter()
        .flat_map(|r| r.refs.iter().map(move |x| (r, x)))
        .filter(|(_, x)| !now.contains(x.entry.root.as_str()))
        .map(|(r, x)| entry_label(&r.repo, Some(&x.ref_name)))
        .collect()
}

/// The other repositories (by index) whose default entry publishes a
/// package the entry profiled as `p` (of repository `own`) uses, or
/// provides a contract it needs ([`RepoDeps::depends_on`]); none with
/// linker version 0.
fn wanted_deps(p: &RepoProfile, own: usize, defaults: &[RepoProfile], version: u32) -> Vec<usize> {
    if version == 0 {
        return Vec::new();
    }
    (0..defaults.len()).filter(|&j| j != own && p.deps.depends_on(&defaults[j].deps)).collect()
}

fn profile_file_name(segment_id: &str) -> String {
    format!("{segment_id}.json")
}

fn read_profile(dir: &Path, file: &str) -> Option<RepoProfile> {
    let bytes = fs::read(dir.join(PROFILES_DIR).join(file)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The tantivy segment of a store segment's embedded symbol index: (uuid,
/// documents); `None` for an empty graph.
type TantivySegment = Option<(String, u32)>;

/// Reads the embedded symbol index's meta of store segment `segment_id`
/// and checks it against the template.
fn tantivy_segment_of(
    bundle: &Bundle,
    meta: &SegmentMeta,
    segment_id: &str,
    root: &str,
    template: &Value,
) -> Result<TantivySegment> {
    let tmeta: Value = serde_json::from_slice(bundle.bytes(&format!("{TANTIVY_PREFIX}{META_FILE}"))?)
        .map_err(|e| corrupt(format!("symbol index meta of {segment_id}: {e}")))?;
    if tmeta.get("schema") != template.get("schema") {
        return Err(SegmentError::Other(format!(
            "the symbol index of {root} (segment {segment_id}) has another schema than this build; re-index it with \
             `rueca index`"
        )));
    }
    let segs = tmeta.get("segments").and_then(Value::as_array).ok_or_else(|| corrupt("symbol index meta segments"))?;
    match segs.as_slice() {
        [] => Ok(None),
        [s] => {
            let sid: SegmentId = serde_json::from_value(s.get("segment_id").cloned().unwrap_or(Value::Null))
                .map_err(|e| corrupt(format!("symbol index segment id of {segment_id}: {e}")))?;
            let docs = s.get("max_doc").and_then(Value::as_u64).ok_or_else(|| corrupt("symbol index max_doc"))?;
            if s.get("deletes").is_some_and(|d| !d.is_null()) || docs != u64::from(meta.node_count) {
                return Err(corrupt(format!("symbol index of {segment_id} does not match its node table")));
            }
            Ok(Some((sid.uuid_string(), docs as u32)))
        }
        _ => Err(corrupt(format!("symbol index of {segment_id} has {} tantivy segments", segs.len()))),
    }
}

/// Copies the files of tantivy segment `uuid` out of `bundle` into the
/// organization index (those already there are kept); their bytes.
fn copy_tantivy_files(bundle: &Bundle, uuid: &str, index_dir: &Path, tmp: &Path) -> Result<u64> {
    let prefix = format!("{TANTIVY_PREFIX}{uuid}.");
    let mut bytes = 0u64;
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
    Ok(bytes)
}

/// Copies the tantivy segment of store segment `segment_id` into the
/// organization index and describes it: (repository id, entry).
fn extract_entry(
    store: &Store,
    dir: &Path,
    tmp: &Path,
    root: &str,
    segment_id: &str,
    generation: u64,
    template: &Value,
) -> Result<(String, OrgEntry)> {
    let bundle = Bundle::open(&store.segment_path(segment_id), FORMAT_VERSION)?;
    let meta: SegmentMeta =
        serde_json::from_slice(bundle.bytes("meta")?).map_err(|e| corrupt(format!("meta of {segment_id}: {e}")))?;
    let (tantivy_segment, docs, index_bytes) = match tantivy_segment_of(&bundle, &meta, segment_id, root, template)? {
        None => (None, 0, 0),
        Some((uuid, docs)) => {
            let bytes = copy_tantivy_files(&bundle, &uuid, &dir.join(INDEX_DIR), tmp)?;
            (Some(uuid), docs, bytes)
        }
    };
    let entry = OrgEntry {
        root: root.to_string(),
        segment_id: segment_id.to_string(),
        snapshot: meta.snapshot.clone(),
        tantivy_segment,
        docs,
        index_bytes,
        since_generation: generation,
        profile: None,
        links: None,
    };
    Ok((meta.snapshot.repo, entry))
}

/// Keeps generations `current` and `current - 1`; deletes older generation
/// files, segment files no kept generation references, and store segments
/// that only dropped generations referenced (default and ref entries
/// alike). Best effort.
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
    let kept_entries = || kept.iter().flat_map(|g| g.repos.iter().flat_map(OrgRepo::entries));
    let live: HashSet<&str> = kept_entries().filter_map(|e| e.tantivy_segment.as_deref()).collect();
    remove_files_where(&dir.join(INDEX_DIR), |name| !live.contains(name.split('.').next().unwrap_or_default()));
    // Profiles and link sets no kept generation names.
    let named: HashSet<&str> = kept_entries()
        .flat_map(|e| e.profile.as_deref().into_iter().chain(e.links.as_ref().and_then(|l| l.file.as_deref())))
        .collect();
    for sub in [PROFILES_DIR, link::LINKS_DIR] {
        remove_files_where(&dir.join(sub), |name| !named.contains(name));
    }
    let catalog: HashSet<String> = store.entries().unwrap_or_default().into_iter().map(|e| e.segment_id).collect();
    let pinned = referenced_segments(&store.dir);
    for e in dropped.iter().flat_map(|g| g.repos.iter().flat_map(OrgRepo::entries)) {
        if !catalog.contains(&e.segment_id) && !pinned.contains(&e.segment_id) {
            let _ = fs::remove_file(store.segment_path(&e.segment_id));
        }
    }
}

/// Deletes the entries of directory `dir` whose file name `doomed`
/// selects. Best effort.
fn remove_files_where(dir: &Path, doomed: impl Fn(&str) -> bool) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        if doomed(ent.file_name().to_string_lossy().as_ref()) {
            let _ = fs::remove_file(ent.path());
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
    /// Segments searched (the `repo` argument), each with its repository:
    /// the repository's default entry or any of its ref entries (several for
    /// a multi-ref query), opened (priors, path globs). Repositories outside
    /// `visible` are ignored.
    pub selected: &'a [(usize, &'a Segment)],
    /// The entries the statistics count for visible repositories that
    /// `selected` does not name (`None`: their default entries). A selected
    /// repository is always counted at its selected segments.
    pub view: Option<&'a OrgSelection>,
}

/// One result of [`OrgIndex::search`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrgHit {
    /// Index into [`OrgGeneration::repos`].
    pub repo: usize,
    pub node: NodeId,
    pub score: f32,
    /// The entry the node belongs to (`slot.repo == repo`).
    pub slot: OrgSlot,
}

/// One generation of an organization's symbol index, opened for reading.
/// `Send + Sync`; a generation never changes, so it can be cached by
/// generation number.
pub struct OrgIndex {
    generation: OrgGeneration,
    store: PathBuf,
    dir: PathBuf,
    reader: IndexReader,
    fields: symbols::Fields,
    /// Every entry of the generation ([`OrgGeneration::slots`] order) and
    /// its position.
    slots: Vec<OrgSlot>,
    position: HashMap<OrgSlot, usize>,
    /// tantivy segment of each entry (`None`: empty graph).
    segments: Vec<Option<SegmentId>>,
    /// Routing profile of each entry (`None`: not computed).
    profiles: Vec<Option<RepoProfile>>,
    docs: route::RepoDocs,
    /// The link table, loaded on first use.
    links: OnceLock<std::result::Result<Arc<LinkTable>, String>>,
    /// Node correspondences of ref entries with their repository's default
    /// entry (link-edge targets only), built on first use.
    remaps: Mutex<HashMap<OrgSlot, Arc<select::Remap>>>,
}

/// The tantivy segment ids of a generation's entries, and the `meta.json`
/// listing each once.
fn index_meta(generation: &OrgGeneration, slots: &[OrgSlot]) -> Result<(Vec<Option<SegmentId>>, Vec<u8>)> {
    let mut segments = Vec::with_capacity(slots.len());
    let mut listed = Vec::new();
    let mut seen = HashSet::new();
    for &slot in slots {
        let e = generation.entry(slot).ok_or_else(|| corrupt("entry out of range"))?;
        let sid = match &e.tantivy_segment {
            Some(u) => Some(
                SegmentId::from_uuid_string(u)
                    .map_err(|err| corrupt(format!("generation {}: segment id {u}: {err}", generation.generation)))?,
            ),
            None => None,
        };
        if let Some(sid) = sid
            && seen.insert(sid)
        {
            listed.push(json!({"segment_id": sid, "max_doc": e.docs, "deletes": null}));
        }
        segments.push(sid);
    }
    let mut meta = generation.tantivy.clone();
    let Value::Object(m) = &mut meta else {
        return Err(corrupt(format!("generation {} has no index settings", generation.generation)));
    };
    m.insert("segments".into(), Value::Array(listed));
    let meta = serde_json::to_vec(&meta).map_err(|e| SegmentError::Other(e.to_string()))?;
    Ok((segments, meta))
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
        let slots = generation.slots();
        let (segments, meta) = index_meta(&generation, &slots)?;
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
            let docs = generation.entry(slots[i]).map_or(0, |e| e.docs);
            if sr.max_doc() != docs || sr.num_docs() != sr.max_doc() {
                return Err(corrupt(format!(
                    "organization index: documents of {} disagree",
                    generation.repos[slots[i].repo].repo
                )));
            }
        }
        let profiles: Vec<Option<RepoProfile>> = slots
            .iter()
            .map(|&s| generation.entry(s).and_then(|e| e.profile.as_deref()).and_then(|f| read_profile(&dir, f)))
            .collect();
        let docs = route::RepoDocs::build(&profiles);
        let position = slots.iter().enumerate().map(|(i, &s)| (s, i)).collect();
        Ok(OrgIndex {
            generation,
            store: store.to_path_buf(),
            dir,
            reader,
            fields,
            slots,
            position,
            segments,
            profiles,
            docs,
            links: OnceLock::new(),
            remaps: Mutex::new(HashMap::new()),
        })
    }

    /// The routing profile of repository `repo`'s default entry (index
    /// into the generation's repositories), if computed.
    pub fn profile(&self, repo: usize) -> Option<&RepoProfile> {
        self.entry_profile(OrgSlot::default_of(repo))
    }

    /// The routing profile of the entry `slot`, if computed.
    pub fn entry_profile(&self, slot: OrgSlot) -> Option<&RepoProfile> {
        self.position.get(&slot).and_then(|&i| self.profiles[i].as_ref())
    }

    /// Every entry of the generation, in index order.
    pub fn slots(&self) -> &[OrgSlot] {
        &self.slots
    }

    /// The link table of this generation (read on first use).
    pub fn links(&self) -> Result<Arc<LinkTable>> {
        self.links
            .get_or_init(|| LinkTable::load(&self.dir, &self.generation.repos).map(Arc::new).map_err(|e| e.to_string()))
            .clone()
            .map_err(SegmentError::Other)
    }

    /// The node correspondence of ref entry `slot` with its repository's
    /// default entry, for the default nodes link edges point at (cached).
    fn remap(&self, slot: OrgSlot, table: &LinkTable) -> Result<Arc<select::Remap>> {
        if let Some(r) = self.remaps.lock().unwrap_or_else(|p| p.into_inner()).get(&slot) {
            return Ok(r.clone());
        }
        let remap = match (table.targets(slot.repo), self.generation.entry(slot)) {
            (Some(targets), Some(e)) => {
                let store = Store { dir: self.store.clone() };
                let default = store.open_segment(&self.generation.repos[slot.repo].default.segment_id)?;
                let at_ref = store.open_segment(&e.segment_id)?;
                select::Remap::build(&default, &at_ref, targets)
            }
            _ => select::Remap::default(),
        };
        let remap = Arc::new(remap);
        self.remaps.lock().unwrap_or_else(|p| p.into_inner()).insert(slot, remap.clone());
        Ok(remap)
    }

    /// Link edges at node `node` of entry `at` in direction `dir`,
    /// optionally of one kind, seen from `view` (RFC 0006 §9.3), whose other
    /// end is a repository in `visible` (RFC 0006 §5: a link edge is
    /// returned only when both ends are visible).
    ///
    /// - Edges of `at`'s own link set (`at` is the consumer: a default entry
    ///   or a ref computed against its dependencies' defaults): the
    ///   dependency end is shown at each entry `view` reads for that
    ///   dependency — its default as stored, a ref **remapped** to the ref's
    ///   node with the same (label, qualified name, path), dropped when the
    ///   ref has none.
    /// - Edges of consumers pointing at `at`'s repository (`at` is the
    ///   dependency): only consumer entries `view` reads; at a ref entry the
    ///   node is first mapped back to the default node the edges were stored
    ///   at (a node the default does not have has none of these edges).
    ///
    /// With the empty view (every repository at its default) this is the
    /// phase 2 link layer unchanged.
    pub fn link_edges(
        &self,
        at: OrgSlot,
        node: NodeId,
        dir: LinkDir,
        kind: Option<EdgeKind>,
        visible: &[usize],
        view: &OrgSelection,
    ) -> Result<Vec<LinkRef>> {
        if !visible.contains(&at.repo) || self.generation.entry(at).is_none() {
            return Ok(Vec::new());
        }
        let table = self.links()?;
        let mut out = Vec::new();
        for s in table.stored(at, node, dir, kind).filter(|s| s.at_consumer) {
            self.push_dependency_end(&mut out, s, &table, visible, view)?;
        }
        let base = match at.ref_idx {
            None => Some(node),
            Some(_) => self.remap(at, &table)?.to_default.get(&node).copied(),
        };
        if let Some(d) = base {
            let consumers = table.stored(OrgSlot::default_of(at.repo), d, dir, kind).filter(|s| !s.at_consumer);
            out.extend(
                consumers
                    .filter(|s| visible.contains(&s.other.repo) && view.contains(s.other))
                    .map(link::Stored::as_ref),
            );
        }
        out.sort_by_key(|e| (e.kind, e.slot, e.node, e.evidence));
        Ok(out)
    }

    /// Adds stored edge `s` (its other end: a dependency's default node) at
    /// each entry `view` reads for that dependency.
    fn push_dependency_end(
        &self,
        out: &mut Vec<LinkRef>,
        s: &link::Stored,
        table: &LinkTable,
        visible: &[usize],
        view: &OrgSelection,
    ) -> Result<()> {
        if !visible.contains(&s.other.repo) {
            return Ok(());
        }
        for slot in view.slots_of(s.other.repo) {
            let node = match slot.ref_idx {
                None => Some(s.node),
                Some(_) => self.remap(slot, table)?.to_ref.get(&s.node).copied(),
            };
            if let Some(node) = node {
                out.push(LinkRef { repo: slot.repo, node, slot, ..s.as_ref() });
            }
        }
        Ok(())
    }

    /// The relevance of every visible repository for `text` (see
    /// [`route`]), best first (ties by repository order), each repository
    /// scored at its default entry. With fewer than two visible
    /// repositories, or a query without terms, every repository gets 1.
    pub fn route(&self, text: &str, visible: &[usize], params: &RouteParams) -> Result<Vec<RepoRoute>> {
        self.route_view(text, visible, &OrgSelection::default(), params)
    }

    /// [`OrgIndex::route`] with each repository scored at the entry `view`
    /// reads first ([`OrgSelection::primary`]): its term distribution and
    /// its own profile (RFC 0006 §9.3: a ref routes with its own profile).
    pub fn route_view(
        &self,
        text: &str,
        visible: &[usize],
        view: &OrgSelection,
        params: &RouteParams,
    ) -> Result<Vec<RepoRoute>> {
        let mut vis: Vec<usize> = visible.iter().copied().filter(|&i| i < self.generation.repos.len()).collect();
        vis.sort_unstable();
        vis.dedup();
        let terms = route::query_terms(text);
        if vis.len() < 2 || terms.is_empty() {
            return Ok(vis.into_iter().map(|repo| RepoRoute { repo, score: 1.0, terms: 1.0, doc: 1.0 }).collect());
        }
        let at: Vec<usize> = vis.iter().map(|&r| self.position_of(view.primary(r))).collect::<Result<_>>()?;
        let (df, cw) = self.term_distribution(&terms, &at)?;
        let by_terms = route::normalize(&route::cori(&df, &cw, params.cori_k, params.cori_c));
        let by_doc = route::normalize(&self.docs.scores(&terms, &at, &params.fields));
        let mut out: Vec<RepoRoute> = vis
            .iter()
            .enumerate()
            .map(|(j, &repo)| RepoRoute {
                repo,
                score: (1.0 - params.mu) * by_terms[j] + params.mu * by_doc[j],
                terms: by_terms[j],
                doc: by_doc[j],
            })
            .collect();
        let best = out.iter().map(|r| r.score).fold(0f32, f32::max);
        if best > 0.0 {
            for r in &mut out {
                r.score /= best;
            }
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.repo.cmp(&b.repo)));
        Ok(out)
    }

    /// The index position of `slot`.
    fn position_of(&self, slot: OrgSlot) -> Result<usize> {
        self.position.get(&slot).copied().ok_or_else(|| SegmentError::NotFound(format!("organization entry {slot:?}")))
    }

    /// Term distribution over the symbol index: documents of each entry
    /// (index positions `at`) holding each term in its name, source or path
    /// (`df[term][j]`), and each entry's documents (`cw[j]`).
    fn term_distribution(&self, terms: &[String], at: &[usize]) -> Result<(Vec<Vec<u64>>, Vec<u64>)> {
        let searcher = self.reader.searcher();
        let readers: HashMap<SegmentId, &tantivy::SegmentReader> =
            searcher.segment_readers().iter().map(|sr| (sr.segment_id(), sr)).collect();
        let fields = symbols::routing_fields(&self.fields);
        let mut df = vec![vec![0u64; at.len()]; terms.len()];
        let mut cw = vec![0u64; at.len()];
        for (j, &p) in at.iter().enumerate() {
            let Some(sr) = self.segments[p].and_then(|sid| readers.get(&sid).copied()) else { continue };
            cw[j] = u64::from(sr.max_doc());
            for &field in &fields {
                let inv = sr.inverted_index(field).map_err(|e| SegmentError::Other(format!("tantivy: {e}")))?;
                for (k, t) in terms.iter().enumerate() {
                    let n = inv
                        .doc_freq(&tantivy::Term::from_field_text(field, t))
                        .map_err(|e| SegmentError::Other(format!("tantivy: {e}")))?;
                    df[k][j] = df[k][j].max(u64::from(n));
                }
            }
        }
        Ok((df, cw))
    }

    pub fn generation(&self) -> &OrgGeneration {
        &self.generation
    }

    /// BM25 search over the selected segments with statistics over the
    /// visible repositories (see the module docs and [`OrgScope`]).
    /// `limit == 0` means 20; `offset` pages the merged ranking. Ordered by
    /// score descending, then index order, then node id.
    pub fn search(&self, query: &SymbolQuery, scope: &OrgScope<'_>) -> Result<Vec<OrgHit>> {
        self.search_with(query, scope, &HashMap::new(), false)
    }

    /// The entry of repository `i` that store segment `seg` is: its default
    /// or one of its refs; an error otherwise.
    fn selected_slot(&self, i: usize, seg: &Segment) -> Result<OrgSlot> {
        let repo = self.generation.repos.get(i).ok_or_else(|| SegmentError::NotFound(format!("repository #{i}")))?;
        let id = &seg.meta().id;
        let slot = repo.slots().find(|(_, e)| e.segment_id == *id).map(|(ref_idx, _)| OrgSlot { repo: i, ref_idx });
        slot.ok_or_else(|| {
            SegmentError::Other(format!(
                "segment {id} is not one of the snapshots organization `{}` holds for {} (default {}{})",
                self.generation.name,
                repo.repo,
                repo.default.segment_id,
                if repo.refs.is_empty() { String::new() } else { format!(", {} refs", repo.refs.len()) }
            ))
        })
    }

    /// The tantivy segments BM25 statistics are summed over: per visible
    /// repository, its selected segments, else the entries `view` reads,
    /// else its default.
    fn stats_segments(
        &self,
        scope: &OrgScope<'_>,
        selected: &HashMap<SegmentId, (OrgSlot, &Segment)>,
    ) -> HashSet<SegmentId> {
        let mut out = HashSet::new();
        for &v in scope.visible {
            let mine: Vec<SegmentId> =
                selected.iter().filter(|(_, (slot, _))| slot.repo == v).map(|(sid, _)| *sid).collect();
            if !mine.is_empty() {
                out.extend(mine);
                continue;
            }
            let slots = scope.view.map_or_else(|| vec![OrgSlot::default_of(v)], |view| view.slots_of(v));
            out.extend(slots.into_iter().filter_map(|s| self.position.get(&s).and_then(|&p| self.segments[p])));
        }
        out
    }

    /// [`OrgIndex::search`] with every score of repository `i` multiplied by
    /// `boost[i]` (missing: 1) inside the top-k: the routing prior
    /// ([`RouteParams::prior`] of [`OrgIndex::route`]). With `local_stats`,
    /// each segment is scored with its own BM25 statistics (as if searched
    /// alone; [`RouteParams::local_stats`]) and the rankings are merged by
    /// score. Any entry of a repository (default or ref) may be selected.
    pub fn search_with(
        &self,
        query: &SymbolQuery,
        scope: &OrgScope<'_>,
        boost: &HashMap<usize, f32>,
        local_stats: bool,
    ) -> Result<Vec<OrgHit>> {
        let limit = symbols::effective_limit(query);
        let glob = query.path_glob.as_deref().map(symbols::compile_glob).transpose()?;
        let mut priors = PriorsBySegment::new();
        let mut by_segment: HashMap<SegmentId, (OrgSlot, &Segment)> = HashMap::new();
        for &(i, seg) in scope.selected {
            let slot = self.selected_slot(i, seg)?;
            if !scope.visible.contains(&i) {
                continue;
            }
            let Some(sid) = self.segments[self.position_of(slot)?] else { continue };
            let docs = self.generation.entry(slot).map_or(0, |e| e.docs);
            if seg.nodes().len() != docs as usize {
                return Err(corrupt(format!(
                    "organization index: node table of {} disagrees",
                    self.generation.repos[i].repo
                )));
            }
            priors.insert(sid, seg.inner.symbols().priors());
            by_segment.insert(sid, (slot, seg));
        }
        let visible = self.stats_segments(scope, &by_segment);
        let searcher = self.reader.searcher();
        let q = symbols::build_query(&self.fields, query);
        let keep = |sid: SegmentId, doc: u32| {
            by_segment.get(&sid).is_some_and(|(_, seg)| {
                (doc as usize) < seg.nodes().len() && glob.as_ref().is_none_or(|g| g.is_match(seg.inner.node_path(doc)))
            })
        };
        let want = query.offset.saturating_add(limit);
        let seg_boost: HashMap<SegmentId, f32> =
            by_segment.iter().filter_map(|(&sid, (slot, _))| boost.get(&slot.repo).map(|&b| (sid, b))).collect();
        let hits = if local_stats {
            let mut all = Vec::new();
            for (sid, p) in priors {
                let own = HashSet::from([sid]);
                let stats = SubsetStats { searcher: &searcher, segments: &own };
                let one: PriorsBySegment = HashMap::from([(sid, p)]);
                all.extend(symbols::top_hits(&searcher, q.as_ref(), &stats, Arc::new(one), &seg_boost, want, &keep)?);
            }
            all.sort_by(|a, b| {
                b.score.total_cmp(&a.score).then(a.segment_ord.cmp(&b.segment_ord)).then(a.doc.cmp(&b.doc))
            });
            all.truncate(want);
            all
        } else {
            let stats = SubsetStats { searcher: &searcher, segments: &visible };
            symbols::top_hits(&searcher, q.as_ref(), &stats, Arc::new(priors), &seg_boost, want, &keep)?
        };
        Ok(hits
            .into_iter()
            .skip(query.offset)
            .take(limit)
            .filter_map(|h| {
                by_segment.get(&h.segment).map(|(slot, _)| OrgHit {
                    repo: slot.repo,
                    node: h.doc,
                    score: h.score,
                    slot: *slot,
                })
            })
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

    /// A generation written by phase 2 (no refs, the entry's fields at the
    /// repository's top level) reads unchanged, and writes back the same
    /// shape.
    #[test]
    fn phase_2_generations_read_unchanged() {
        let text = r#"{
          "format": 1, "name": "acme", "generation": 4, "created_at": 1700000000,
          "repos": [{
            "repo": "local/billing", "root": "/src/billing", "segment_id": "seg-a",
            "snapshot": {"repo": "local/billing", "commit": "0123456789abcdef", "dirty": false,
                         "branch": "main", "root": "/src/billing", "indexed_at": 1700000000,
                         "rueca_version": "0.1.0"},
            "tantivy_segment": "8f7c0e5b2d6a4c1e9b3f0a7d5c2e8b1f", "docs": 12, "index_bytes": 4096,
            "since_generation": 3, "profile": "seg-a.json",
            "links": {"deps": [{"repo": "local/pay", "segment_id": "seg-b"}], "edges": 2, "file": "seg-a-x.bin"}
          }],
          "tantivy": {"segments": []}, "linker": 1
        }"#;
        let g: OrgGeneration = serde_json::from_str(text).unwrap();
        let r = &g.repos[0];
        assert_eq!((r.repo.as_str(), r.default.root.as_str(), r.default.docs), ("local/billing", "/src/billing", 12));
        assert_eq!(r.default.since_generation, 3);
        assert_eq!(r.default.links.as_ref().unwrap().deps[0].repo, "local/pay");
        assert!(r.refs.is_empty());
        assert_eq!(g.slots(), [OrgSlot::default_of(0)]);
        assert_eq!(g.ref_name(OrgSlot::default_of(0)).as_deref(), Some("main"));
        let back = serde_json::to_value(&g).unwrap();
        assert_eq!(back["repos"][0]["segment_id"], "seg-a");
        assert!(back["repos"][0].get("refs").is_none() && back["repos"][0].get("default").is_none());
        assert_eq!(serde_json::from_value::<OrgGeneration>(back).unwrap(), g);
    }

    #[test]
    fn template_has_schema_and_no_segments() {
        let t = tantivy_template().unwrap();
        assert!(t["schema"].as_array().is_some_and(|a| !a.is_empty()));
        assert_eq!(t["segments"], json!([]));
        assert!(t.get("index_settings").is_some());
    }
}
