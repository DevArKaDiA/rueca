//! Rueca service mode (RFC 0008 §5, §7, §10): a **service store** in
//! object storage that indexers publish to and replicas pull from, behind
//! the [`rueca_storage::Storage`] seam (phase 0), and the indexing pipeline
//! that keeps it current (phase 2): [`jobs`] and their leases, the
//! [`reconcile`]r with adaptive polling over [`remote`] code hosts, the
//! [`indexer`] job runner and the [`updater`] leader loop with idle expiry
//! from [`reads`].
//!
//! # The mirror model
//!
//! Nothing queries the bucket directly. Every role keeps a **local store
//! directory** with today's layout ([`rueca_segment::Store`]) as a mirror
//! of the part of the bucket it needs, so indexing and querying run the
//! local code unchanged:
//!
//! - an **indexer** indexes a snapshot into its local store as in local
//!   mode, then [`publish_snapshot`] uploads the snapshot's packs and
//!   segment and records it in the repository catalog by compare-and-swap;
//! - a **replica** [`pull_repo`]s (and [`pull_generation`]s): it downloads
//!   segments and packs into its local store and registers them in the
//!   local `catalog.json` (and installs the organization generation), so
//!   [`rueca_query::Engine`](https://docs.rs/rueca-query) opens them exactly
//!   as in local mode;
//! - the **updater** pulls, runs today's
//!   [`rueca_segment::org::update_org_with`] on its mirror and
//!   [`publish_generation`]s the result; it also runs
//!   [`collect_garbage`].
//!
//! `packs/index.rcpi` stays local-only: a mirror's blob index is rebuilt
//! from the pack footers whenever its pack set changed (`BlobIndex` in
//! rueca-segment). A snapshot names its packs itself (`SegmentMeta::packs`,
//! repeated in the catalog), so the bucket needs no blob index.
//!
//! ## Synthetic roots
//!
//! The local catalog keys entries by root directory, and the bucket has no
//! roots (RFC 0008 §5.1). A mirror registers a pulled snapshot under a
//! **synthetic root** inside the mirror ([`ServiceStore::mirror_root`]):
//!
//! | Pulled | Root | Catalog entry |
//! |---|---|---|
//! | the repository's default ref | `<mirror>/repos/<repo-dir>` | working-tree entry ([`rueca_segment::Store::register`]) |
//! | any other ref | `<mirror>/refs/<repo-dir>/<ref-slug>` (= [`rueca_index::refs::export_dir`]) | ref snapshot ([`rueca_segment::Store::register_ref`]) |
//!
//! The default root is not under `refs/`, so local-mode selection treats it
//! as the repository's working tree (a query without `repo` reads it); the
//! ref roots are exactly where `rueca index --ref` would export the ref, so
//! [`rueca_segment::org::update_org_with`] lists them as the member's ref
//! entries (it only takes ref snapshots under `<store>/refs/`), and a
//! mirror that is also an indexer replaces its own export of the same ref.
//! The directories are never created: roots are only catalog keys
//! (`canonical` resolves them against the canonical mirror directory). The
//! mirror directory should be dedicated to the service store: a working
//! tree indexed into it is a second default entry of its repository.
//!
//! A pulled generation's entries are rewritten to the mirror's synthetic
//! roots before it is installed, so the mirror's own `update_org_with`
//! (members: [`ServiceStore::mirror_members`]) reuses every unchanged entry
//! by (root, segment) instead of rebuilding the organization.
//!
//! # Object layout (RFC 0008 §5.1)
//!
//! ```text
//! store.json                                   {format, created_at, org}        Create once
//! repos/<repo-dir>/catalog.json                RepoCatalog                      CAS
//! repos/<repo-dir>/segments/<id>.rcseg         segment bundle                   Create
//! repos/<repo-dir>/packs/<id>.rcpack           content pack                     Create
//! orgs/<name>/org.json                         {name, generation}               CAS
//! orgs/<name>/generations/<n:010>.json         OrgGeneration                    Create
//! orgs/<name>/index/<uuid>.<ext>               tantivy segment files            Create
//! orgs/<name>/profiles/<segment>.json          RepoProfile                      Create
//! orgs/<name>/links/<segment>-<hash>.bin       link set                         Create
//! gc/candidates.json                           deletion candidates (two rounds) CAS
//! jobs/<repo-dir>.json                         Job (desired state, lease)       CAS
//! leases/org-updater.json                      UpdaterLease                     CAS
//! reads/<yyyy-mm-dd>/<hh>-<replica>.json       ReadsObject                      Create
//! ```
//!
//! `<repo-dir>` is [`repo_dir`]: the slug-and-hash directory name of
//! `rueca index --ref` exports ([`rueca_index::refs::repo_dir_name`]). The
//! service keys repositories by id (no path prefix: there are no roots).
//! A pack holds the blobs of every snapshot its local store wrote while it
//! was open; it is uploaded under every repository whose snapshot lists it.
//! Facts and vectors in the bucket (§5.1) come in phase 3.
//!
//! # Sequences
//!
//! **Publish** ([`publish_snapshot`], §7.9): (1) every pack the snapshot
//! lists that the repository directory does not hold yet, then the segment,
//! with `Create` (`AlreadyExists` is fine: a pack shared with an earlier
//! snapshot, or a re-run); (2) [`rueca_storage::cas_update`] of the
//! catalog: when the ref already has the snapshot's commit (a clean
//! snapshot) the step is [`rueca_storage::CasStep::Unchanged`] and the
//! report says `duplicate` (the uploaded segment is left to garbage
//! collection); otherwise the snapshot goes to the head of its ref and the
//! ref is trimmed to [`Retention`]; (3) the packs and segment are checked
//! again and re-uploaded if a concurrent garbage collection removed one
//! between (1) and (2).
//!
//! **Pull** ([`pull_repo`]): read the catalog; per selected ref, its head
//! snapshot: the segment is downloaded into `<mirror>/tmp/` and opened (the
//! full bundle validation: trailer, footer and every component checksum),
//! its packs are downloaded (size and magic checked), then the segment is
//! renamed into `segments/` and registered under its synthetic root.
//! Files already in the mirror are kept (they were installed only after
//! the same checks). Packs go first, so a mirror never holds a segment
//! without its packs.
//!
//! **Generation** ([`publish_generation`], §5.4): after `update_org_with`
//! built generation `n` on the mirror, (1) `generations/<n>.json` with
//! `Create`; `AlreadyExists` with another content (roots aside) means
//! another updater published `n`: [`Conflict`]; (2) the `index/`,
//! `profiles/` and `links/` files it names, `Create` (`AlreadyExists` is
//! fine); (3) `org.json` by CAS, expecting `n − 1` (the generation the build
//! started from; an absent pointer accepts any), else [`Conflict`]. There
//! is no inline garbage collection. [`pull_generation`] does the reverse
//! with a conditional GET of `org.json`: it downloads the generation, every
//! segment and pack it names and its organization files, then writes the
//! generation file and, last, the local `org.json`, under the mirror's
//! store lock ([`rueca_segment::Store::lock`]), so a local engine reopens
//! it on its next call.
//!
//! **Garbage collection** ([`collect_garbage`], §5.8): one listing of
//! `repos/` and `orgs/`; then, per catalog, a CAS that expires refs
//! (pull requests closed for [`Retention::pr_after_close`] move to
//! `removed`) and re-trims snapshots; **mark**: every snapshot of every
//! catalog (segment and packs), and the current and previous generation of
//! every organization with every `index/`, `profiles/`, `links/` object,
//! segment and pack they name (a segment no catalog lists any more has its
//! packs read from its `meta` component by three range reads); **sweep, in
//! two rounds** (`gc/candidates.json`): an unmarked object of an immutable
//! prefix older than [`Retention::grace`] (by
//! [`rueca_storage::ObjectMeta::last_modified`]; no timestamp: kept) is
//! only recorded as a **candidate** with the time it was first seen; a
//! later collection that finds it still unreferenced at least `grace`
//! after that time commits it (a CAS of the list, which fails over to
//! "keep" when a publisher took it back) and deletes it. A candidate that
//! is referenced again leaves the list. Catalogs, pointers and
//! `store.json` are never deleted. **A zero grace is a single round**: the
//! unreferenced objects are deleted at once (each after a re-read of its
//! catalog or pointer); tests use it, a deployment with concurrent
//! publishers must not.
//!
//! The grace period is what makes collection safe against in-flight work:
//! a publish uploads before its catalog CAS, and a replica may still be
//! downloading the previous generation.
//!
//! **Invariant: a referenced object is present.** Before its uploads, a
//! publish (snapshot or generation) **rescues** every object it names: one
//! CAS of `gc/candidates.json` takes them off the candidate list; one
//! already committed for deletion makes it wait until it is deleted, then
//! it is uploaded again. A rescued object can be recorded again only by a
//! collection that marks after the rescue, and committed only by one that
//! marks a grace after that, when the publisher's catalog CAS has landed
//! and the mark sees it live. The publish then uploads every object the
//! store lacks (not only the pack it just wrote), and lists them again
//! after the CAS, re-uploading any that is missing. **Assumption: a
//! publish completes, rescue to catalog CAS, within the grace period**
//! (24 h by default; seconds in practice). Under it the race between a
//! publish reusing an old unreferenced pack and a collection is closed.
//!
//! A generation names the segments it was built from; [`publish_generation`]
//! refuses one whose segments are not in the store, so the updater builds
//! only from pulled (published) snapshots.
//!
//! # The indexing pipeline (phase 2)
//!
//! Level-triggered (§7.2): the desired state of a repository lives in its
//! job object and everything converges on it.
//!
//! - [`jobs`]: `jobs/<repo-dir>.json` — desired refs, state, priority,
//!   lease (15 min, renewed every 5 min, stolen 30 s after it ends, epoch
//!   incremented), attempts and back-off (`min(10 min × attempts, 120
//!   min)`), the polling schedule. [`jobs::lease_job`],
//!   [`jobs::LeasedJob::renew`] / `finish` / `fail`,
//!   [`jobs::desired_update`] (polling, webhooks and admin requests merge
//!   desired commits by CAS).
//! - [`remote`]: the `[indexing].hosts` allowlist, `git` with credentials
//!   in the child's environment (`GIT_CONFIG_COUNT`, never the command
//!   line), `git ls-remote --symref`.
//! - [`reconcile`]: inventory (`[[repo]]` → catalog and job), adaptive
//!   polling (half the quiet time, 45 s – 8 h, 8 per host, a `429` pauses
//!   the host), desired refs from the default branch, branch and tag globs
//!   and pull request heads, branch removal and pull request closing,
//!   priorities.
//! - [`indexer`]: [`indexer::run_job`] runs a leased job's refs (default
//!   first) through a [`indexer::RefRunner`] into the indexer's staging
//!   store, then [`publish_snapshot`]; [`indexer::InProcessRunner`] is a
//!   stub until the child-process runner of phase 2 Part B.
//! - [`updater`]: the leader of `leases/org-updater.json` reconciles, runs
//!   idle expiry ([`reads`]: pull requests and on-demand refs unread for
//!   [`Retention::pr_idle`]) and [`collect_garbage`] hourly, pulls changed
//!   repositories and publishes generations.
//!
//! # Phases
//!
//! - Phase 1 (`rueca serve`): replicas poll [`pull_generation`] with the
//!   last [`PullGenerationReport::version`] (one conditional request per
//!   tick) and serve MCP over HTTP from the mirror.
//! - Phase 2 Parts B and C: bare git mirrors, filtered fetches and the
//!   `rueca index-worker` child process behind [`indexer::RefRunner`];
//!   webhooks into [`jobs::desired_update`] (setting
//!   [`RefEntry::closed_at`] on close events).
//! - Phase 3: `vectors` ([`SnapshotEntry::vectors`]) and their objects,
//!   `embed` jobs ([`jobs::JobKind::Embed`]), facts in the bucket.

mod candidates;
mod catalog;
mod gc;
mod generation;
pub mod indexer;
pub mod jobs;
mod publish;
mod pull;
pub mod reads;
pub mod reconcile;
pub mod remote;
mod status;
pub mod updater;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use bytes::Bytes;
use rueca_segment::Store;
use rueca_segment::org::OrgMember;
use rueca_storage::{PutError, Storage};
use serde::{Deserialize, Serialize};

pub use catalog::{CATALOG_FORMAT, RefEntry, RefKind, Removed, RepoCatalog, Retention, SnapshotEntry, VectorsEntry};
pub use gc::{GcReport, collect_garbage};
pub use generation::{Conflict, GenerationReport, PullGenerationReport, publish_generation, pull_generation};
pub use publish::{PublishReport, publish_snapshot};
pub use pull::{PullReport, PullWhich, PulledRef, pull_repo};
pub use status::{ClassUsage, RefStatus, RepoStatus, ServiceStatus, object_class, status};

/// Version of `store.json`.
pub const STORE_FORMAT: u32 = 1;

/// Compare-and-swap attempts of every mutable object (RFC 0008 §5.2).
pub(crate) const CAS_ATTEMPTS: u32 = 8;

const STORE_FILE: &str = "store.json";

/// `store.json`: written once, when the store is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreInfo {
    pub format: u32,
    /// Unix seconds.
    pub created_at: u64,
    /// The organization the deployment serves (RFC 0008 §6.9).
    pub org: String,
}

/// A service store and the local store that mirrors it.
pub struct ServiceStore {
    pub storage: Arc<dyn Storage>,
    /// The mirror: a normal local store (canonical directory).
    pub local: Store,
    /// The organization of the store (`store.json`).
    pub org: String,
}

impl ServiceStore {
    /// Opens the store at `url` (see [`rueca_storage::open`]) for
    /// organization `org`, with `local_dir` as the mirror (created if
    /// needed). Creates `store.json` (`Create`) when the store is new; an
    /// existing store must be `org`'s.
    pub fn open(
        url: &str,
        options: &BTreeMap<String, String>,
        local_dir: &Path,
        org: &str,
    ) -> anyhow::Result<ServiceStore> {
        rueca_segment::org::validate_name(org)?;
        let storage = rueca_storage::open(url, options)?;
        let info = ensure_store_info(storage.as_ref(), org)?;
        if info.org != org {
            bail!("store `{url}` serves organization `{}`, not `{org}`", info.org);
        }
        Self::with_storage(storage, local_dir, org)
    }

    /// Opens an existing store at `url`, for the organization its
    /// `store.json` names.
    pub fn connect(url: &str, options: &BTreeMap<String, String>, local_dir: &Path) -> anyhow::Result<ServiceStore> {
        let storage = rueca_storage::open(url, options)?;
        let Some(info) = read_store_info(storage.as_ref())? else {
            bail!("`{url}` is not a Rueca service store (no {STORE_FILE}); create it by publishing with --org <name>");
        };
        Self::with_storage(storage, local_dir, &info.org)
    }

    /// [`ServiceStore::open`] over an already opened backend: creates
    /// `store.json` for `org` when absent; an existing store must be
    /// `org`'s.
    pub fn open_with_storage(storage: Arc<dyn Storage>, local_dir: &Path, org: &str) -> anyhow::Result<ServiceStore> {
        rueca_segment::org::validate_name(org)?;
        let info = ensure_store_info(storage.as_ref(), org)?;
        if info.org != org {
            bail!("the store serves organization `{}`, not `{org}`", info.org);
        }
        Self::with_storage(storage, local_dir, org)
    }

    /// A service store over an already opened backend (tests wrap one).
    /// `store.json` is neither read nor written.
    pub fn with_storage(storage: Arc<dyn Storage>, local_dir: &Path, org: &str) -> anyhow::Result<ServiceStore> {
        rueca_segment::org::validate_name(org)?;
        fs::create_dir_all(local_dir).with_context(|| format!("creating {}", local_dir.display()))?;
        let dir = fs::canonicalize(local_dir).with_context(|| format!("opening {}", local_dir.display()))?;
        let local = Store::open(&dir)?;
        Ok(ServiceStore { storage, local, org: org.to_string() })
    }

    /// The synthetic root under which the mirror registers ref `ref_name`
    /// of repository `repo` (`None`: its default ref). See the crate docs.
    pub fn mirror_root(&self, repo: &str, ref_name: Option<&str>) -> PathBuf {
        match ref_name {
            None => self.local.dir.join("repos").join(repo_dir(repo, None)),
            Some(r) => rueca_index::refs::export_dir(&self.local.dir, repo, "", r),
        }
    }

    /// The organization members of the mirror: every default entry pulled
    /// into it (root under `<mirror>/repos/`), for
    /// [`rueca_segment::org::update_org_with`]. Their ref entries are found
    /// by `update_org_with` itself.
    pub fn mirror_members(&self) -> anyhow::Result<Vec<OrgMember>> {
        let repos = self.local.dir.join("repos");
        Ok(self
            .local
            .entries()?
            .into_iter()
            .filter(|e| !e.is_ref_export() && Path::new(&e.root).starts_with(&repos))
            .map(|e| OrgMember { root: e.root, ..OrgMember::default() })
            .collect())
    }

    /// Object path of a repository directory's file (`catalog.json`,
    /// `segments/<id>.rcseg`, …).
    pub(crate) fn repo_path(rd: &str, rest: &str) -> String {
        format!("repos/{rd}/{rest}")
    }

    /// Object path under `orgs/<org>/`.
    pub(crate) fn org_path(&self, rest: &str) -> String {
        format!("orgs/{}/{rest}", self.org)
    }
}

/// The directory name of repository `repo_id` in a service store
/// (`repos/<repo-dir>/`): the slug-and-hash scheme of ref exports
/// ([`rueca_index::refs::repo_dir_name`]); `prefix` is the path of a
/// subdirectory root inside the repository (the service passes `None`).
pub fn repo_dir(repo_id: &str, prefix: Option<&str>) -> String {
    rueca_index::refs::repo_dir_name(repo_id, prefix.unwrap_or(""))
}

fn read_store_info(storage: &dyn Storage) -> anyhow::Result<Option<StoreInfo>> {
    let Some(v) = storage.get(STORE_FILE)? else { return Ok(None) };
    let info: StoreInfo = serde_json::from_slice(&v.value).with_context(|| format!("{STORE_FILE} is not valid"))?;
    if info.format != STORE_FORMAT {
        bail!("{STORE_FILE} has format {} (this build reads {STORE_FORMAT})", info.format);
    }
    Ok(Some(info))
}

/// Reads `store.json`, creating it for `org` when absent (`Create`: when
/// two processes create it at once, the loser reads the winner's).
fn ensure_store_info(storage: &dyn Storage, org: &str) -> anyhow::Result<StoreInfo> {
    if let Some(info) = read_store_info(storage)? {
        return Ok(info);
    }
    let info = StoreInfo { format: STORE_FORMAT, created_at: unix_now(), org: org.to_string() };
    match storage.put_new(STORE_FILE, Bytes::from(serde_json::to_vec_pretty(&info)?)) {
        Ok(_) => Ok(info),
        Err(PutError::AlreadyExists) => read_store_info(storage)?.context("store.json vanished after a create race"),
        Err(e) => Err(anyhow::Error::from(e).context(format!("creating {STORE_FILE}"))),
    }
}

pub(crate) fn unix_now() -> u64 {
    unix_secs(SystemTime::now())
}

pub(crate) fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Creates an immutable object from a local file; `AlreadyExists` is
/// success (`false`). Returns whether it was uploaded.
pub(crate) fn put_file_once(storage: &dyn Storage, path: &str, src: &Path) -> anyhow::Result<bool> {
    match storage.put_new_file(path, src) {
        Ok(_) => Ok(true),
        Err(PutError::AlreadyExists) => Ok(false),
        Err(e) => Err(anyhow::Error::from(e).context(format!("uploading {} to {path}", src.display()))),
    }
}

/// Writes `bytes` to `dest` through a temporary file in `tmp_dir`
/// (fsynced, then renamed).
pub(crate) fn write_atomic(dest: &Path, tmp_dir: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = tempfile::Builder::new().prefix(".service-").tempfile_in(tmp_dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(dest).map_err(|e| e.error).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}
