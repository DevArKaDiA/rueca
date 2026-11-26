//! The organization link layer, packages and contracts (RFC 0006 §3.3): the
//! storage side.
//!
//! Computing cross-repository edges needs the language packs and the
//! resolver, which live above this crate; they come in through
//! [`OrgLinker`] (`rueca-index` implements it). This module keeps what is
//! language-neutral:
//!
//! - **Package registry**: every repository's [`RepoDeps`] lists the
//!   [`Package`]s its manifests publish (with the import keys that name
//!   them), the import keys it could not resolve inside itself, and the
//!   packages its manifests require. A repository **depends on** another
//!   when one of its import keys is a key of a package the other publishes
//!   (`key == import`, or `import` continues `key` after the ecosystem's
//!   separator: `go:github.com/a/lib` covers `go:github.com/a/lib/sub`), or
//!   when it requires one of those packages by name.
//! - **Contract keys** (RFC 0006 phase 4): every repository's [`RepoDeps`]
//!   also lists the distinct keys of its segment contract table
//!   ([`ContractKey`]). A repository also depends on another when one of
//!   its `Consumes` keys matches a `Provides` key of the other
//!   (`rueca_model::contract::match_keys`), or one of its code `Provides`
//!   keys matches a specification `Provides` key of the other (a server
//!   implementing a spec published by another repository). Contract link
//!   edges (`meta.strategy = Contract`) go into the same link sets.
//! - **Link sets**: the edges a consumer has into its dependencies, one
//!   file per (consumer segment, dependency segments, linker version):
//!   `links/<consumer segment>-<hash>.bin` = `zstd(postcard(Vec<LinkEdge>))`.
//!   A generation records, per repository, its link file and the
//!   dependencies (repository + store segment) it was computed against, so
//!   an update recomputes exactly the consumers whose own segment or one of
//!   whose dependencies changed (the reverse dependencies of a re-indexed
//!   repository), or whose dependency set changed (a new repository
//!   publishing a package it imports).
//! - **Link table**: the link edges of a generation, by endpoint, for
//!   `trace` ([`super::OrgIndex::link_edges`]).
//! - **Refs** (RFC 0006 §9.3): a ref entry of a repository is a consumer of
//!   its own — its link set is computed against its dependencies' default
//!   entries, from the imports of the ref's own segment, and versioned the
//!   same way (its own segment in the file name). A change of a ref never
//!   recomputes another repository's links: link sets only point at
//!   default entries.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use rueca_model::contract::{match_bucket, match_keys};
use rueca_model::{ContractKind, ContractRow, ContractSide, EdgeKind, EdgeMeta, NodeId};
use serde::{Deserialize, Serialize};

use super::{OrgRepo, OrgSlot, corrupt};
use crate::store::atomic_write;
use crate::{Result, Segment, SegmentError, Store};

pub(crate) const LINKS_DIR: &str = "links";

/// A package a repository publishes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Ecosystem: `npm`, `pypi`, `go`, `maven`, `composer`, `c`.
    pub eco: String,
    /// Published name: `@vue/runtime-core`, `click`, `github.com/gin-gonic/gin`,
    /// `com.google.code.gson:gson`, `nesbot/carbon`, `fmt`.
    pub name: String,
    /// The package's root directory in the repository (`""`: the root).
    pub dir: String,
    /// Import keys that name the package (see [`import_matches`]):
    /// `npm:@vue/runtime-core`, `py:click`, `go:github.com/gin-gonic/gin`,
    /// `java:com.google.gson`, `php:Carbon`, `c:fmt`.
    pub keys: Vec<String>,
}

/// What a repository publishes and what it needs from outside, as computed
/// by [`OrgLinker::analyze`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoDeps {
    #[serde(default)]
    pub packages: Vec<Package>,
    /// Import keys the repository uses and does not resolve inside itself,
    /// sorted and deduplicated (`npm:vue`, `py:click`, `php:Carbon\Carbon`).
    #[serde(default)]
    pub imports: Vec<String>,
    /// Packages its manifests depend on, `eco:name` (`composer:nesbot/carbon`).
    #[serde(default)]
    pub requires: Vec<String>,
    /// The contract keys of its contract table (RFC 0006 phase 4), sorted
    /// and deduplicated: what it provides and what it consumes.
    #[serde(default)]
    pub contracts: Vec<ContractKey>,
}

/// One distinct contract key of a repository ([`RepoDeps::contracts`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContractKey {
    pub side: ContractSide,
    pub kind: ContractKind,
    pub key: String,
    /// A `Provides` key from a specification (an OpenAPI operation, a
    /// `.proto` RPC method), not from code.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spec: bool,
}

impl ContractKey {
    /// The distinct keys of a contract table, sorted.
    pub fn of_rows(rows: &[ContractRow]) -> Vec<ContractKey> {
        let mut keys: Vec<ContractKey> = rows
            .iter()
            .map(|r| ContractKey {
                side: r.side,
                kind: r.kind,
                key: r.key.clone(),
                spec: r.side == ContractSide::Provides && r.is_spec(),
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Whether this key makes its repository depend on a repository
    /// providing `provider`: a consumed key matching it, or a code
    /// provider matching a specification (`IMPLEMENTS`).
    fn needs(&self, provider: &ContractKey) -> bool {
        let wanted = match self.side {
            ContractSide::Consumes => true,
            ContractSide::Provides => !self.spec && provider.spec,
        };
        wanted && match_keys(self.kind, &provider.key, &self.key).is_some()
    }
}

/// Separator after which an import key continues a package key.
fn key_separator(key: &str) -> &'static str {
    match key.split_once(':').map_or("", |(eco, _)| eco) {
        "py" | "java" | "cs" => ".",
        "php" => "\\",
        _ => "/",
    }
}

/// Whether import key `import` names something in package key `key`: the
/// same key, or `key` followed by the ecosystem's separator
/// (`go:github.com/a/lib` → `go:github.com/a/lib/sub`; `php:Carbon` →
/// `php:Carbon\Carbon`; `py:click` → `py:click.core`).
pub fn import_matches(key: &str, import: &str) -> bool {
    let sep = key_separator(key);
    let key = key.strip_suffix(sep).unwrap_or(key);
    import == key || import.strip_prefix(key).is_some_and(|rest| rest.starts_with(sep))
}

/// Normalized package name for `requires` matching: lowercase; PyPI also
/// folds `_` and `.` into `-` (PEP 503).
pub fn normalize_package(eco: &str, name: &str) -> String {
    let lower = name.trim().to_lowercase();
    if eco == "pypi" { lower.replace(['_', '.'], "-") } else { lower }
}

impl RepoDeps {
    /// Whether this repository depends on repository `other`: it imports or
    /// requires a package `other` publishes
    /// ([`RepoDeps::depends_on_packages`]), or one of its contract keys
    /// needs a key `other` provides ([`RepoDeps::depends_on_contracts`]).
    pub fn depends_on(&self, other: &RepoDeps) -> bool {
        self.depends_on_packages(&other.packages) || self.depends_on_contracts(other)
    }

    /// Whether this repository depends on a repository publishing `published`.
    pub fn depends_on_packages(&self, published: &[Package]) -> bool {
        published.iter().any(|p| {
            p.keys.iter().any(|k| {
                // Imports are sorted: only those sharing the key's
                // ecosystem prefix can match.
                let eco = k.split_once(':').map_or("", |(e, _)| e);
                let from = self.imports.partition_point(|i| i.as_str() < k.as_str());
                self.imports[from..].iter().take_while(|i| i.starts_with(eco)).any(|i| import_matches(k, i))
            }) || self.requires.iter().any(|r| *r == format!("{}:{}", p.eco, normalize_package(&p.eco, &p.name)))
        })
    }
}

impl RepoDeps {
    /// Whether one of this repository's `Consumes` keys matches a
    /// `Provides` key of `other`, or one of its code `Provides` keys
    /// matches a specification `Provides` key of `other` (a server
    /// implementing a spec published elsewhere).
    pub fn depends_on_contracts(&self, other: &RepoDeps) -> bool {
        let needs = self.contracts.iter().filter(|k| k.side == ContractSide::Consumes || !k.spec);
        let mut needs = needs.peekable();
        if needs.peek().is_none() {
            return false;
        }
        let provided = provider_index(&other.contracts);
        if provided.is_empty() {
            return false;
        }
        needs.any(|k| {
            let bucket = match_bucket(k.kind, &k.key);
            provided.get(&bucket).into_iter().flatten().any(|p| k.needs(p))
        })
    }
}

/// The `Provides` keys of a key list by [`match_bucket`].
fn provider_index(keys: &[ContractKey]) -> HashMap<(ContractKind, String), Vec<&ContractKey>> {
    let mut out: HashMap<(ContractKind, String), Vec<&ContractKey>> = HashMap::new();
    for k in keys.iter().filter(|k| k.side == ContractSide::Provides) {
        out.entry(match_bucket(k.kind, &k.key)).or_default().push(k);
    }
    out
}

/// One dependency a link set was computed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgDep {
    pub repo: String,
    pub segment_id: String,
}

/// The link set of a consumer repository in a generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgLinks {
    /// File name under `links/` (`None`: computed, no edges, or failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Dependencies (sorted by repository id) the edges point into.
    pub deps: Vec<OrgDep>,
    pub edges: u32,
    /// Why linking failed, if it did (the repository then has no links).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A cross-repository edge between a consumer node and a node of one of its
/// dependencies, observed in the consumer (`meta.evidence` names a file of
/// the consumer). Most edges leave the consumer (`IMPORTS`, `CALLS`,
/// `INHERITS`, …); a few kinds point into it (`DECORATES`: a library's
/// decorator decorates the consumer's definition), marked `reversed`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinkEdge {
    /// Node of the consumer's segment.
    pub src: NodeId,
    /// Index into [`OrgLinks::deps`].
    pub dep: u32,
    /// Node of that dependency's segment.
    pub dst: NodeId,
    pub kind: EdgeKind,
    pub meta: EdgeMeta,
    /// The edge goes from `dst` (dependency) to `src` (consumer).
    pub reversed: bool,
}

/// One side of a link computation: a repository of the generation being
/// built, the opened segment of one of its entries (a consumer: its default
/// or one of its refs; a dependency: always its default) and what that
/// segment publishes / imports.
pub struct LinkInput<'a> {
    pub repo: &'a OrgRepo,
    pub segment: &'a Segment,
    pub deps: &'a RepoDeps,
}

/// Computes what the organization layer cannot compute without language
/// packs: package registries and cross-repository edges. `rueca-index`
/// implements it with the language packs and the resolver.
pub trait OrgLinker: Sync {
    /// Version of what [`OrgLinker::analyze`] and [`OrgLinker::link`]
    /// produce; `0` means "no linker" (no packages, no links). A new
    /// version recomputes every repository's analysis and link set.
    fn version(&self) -> u32;
    /// Packages, unresolved imports and required packages of one segment.
    fn analyze(&self, store: &Store, segment: &Segment) -> Result<RepoDeps>;
    /// The edges from `consumer` into `deps` (`LinkEdge::dep` indexes
    /// `deps`).
    fn link(&self, store: &Store, consumer: &LinkInput<'_>, deps: &[LinkInput<'_>]) -> Result<Vec<LinkEdge>>;
}

/// The linker of an organization without language packs: no packages, no
/// links (the routing profile is still computed).
pub struct NoLinker;

impl OrgLinker for NoLinker {
    fn version(&self) -> u32 {
        0
    }
    fn analyze(&self, _: &Store, _: &Segment) -> Result<RepoDeps> {
        Ok(RepoDeps::default())
    }
    fn link(&self, _: &Store, _: &LinkInput<'_>, _: &[LinkInput<'_>]) -> Result<Vec<LinkEdge>> {
        Ok(Vec::new())
    }
}

/// Link file name of a consumer segment and its dependencies.
pub(crate) fn link_file_name(consumer_segment: &str, deps: &[OrgDep], linker: u32) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&linker.to_le_bytes());
    for d in deps {
        h.update(d.repo.as_bytes());
        h.update(&[0]);
        h.update(d.segment_id.as_bytes());
        h.update(&[0]);
    }
    let hex = h.finalize().to_hex();
    format!("{consumer_segment}-{}.bin", &hex[..16])
}

pub(crate) fn write_links(dir: &Path, tmp: &Path, name: &str, edges: &[LinkEdge]) -> Result<()> {
    let raw = postcard::to_stdvec(edges).map_err(|e| SegmentError::Other(format!("links: {e}")))?;
    let bytes = zstd::encode_all(raw.as_slice(), 3)?;
    atomic_write(&dir.join(LINKS_DIR).join(name), tmp, &bytes)
}

pub(crate) fn read_links(dir: &Path, name: &str) -> Result<Vec<LinkEdge>> {
    let bytes = fs::read(dir.join(LINKS_DIR).join(name))?;
    let raw = zstd::decode_all(bytes.as_slice()).map_err(|e| corrupt(format!("links {name}: {e}")))?;
    postcard::from_bytes(&raw).map_err(|e| corrupt(format!("links {name}: {e}")))
}

/// Direction of a link lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDir {
    /// Edges leaving the node (the node is in the consumer).
    Out,
    /// Edges arriving at the node (the node is in a dependency).
    In,
}

/// One link edge seen from one of its endpoints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkRef {
    /// The other endpoint: repository index in the generation and node (of
    /// the entry `slot`).
    pub repo: usize,
    pub node: NodeId,
    pub kind: EdgeKind,
    pub meta: EdgeMeta,
    /// Repository whose file table `meta.evidence` refers to (the consumer).
    pub evidence_repo: usize,
    /// The other endpoint's entry: `repo`'s default or one of its refs.
    pub slot: OrgSlot,
    /// The consumer entry whose file table `meta.evidence` refers to.
    pub evidence: OrgSlot,
}

/// One link edge stored at one of its endpoints: the other endpoint, and
/// whether the endpoint it is stored at is the consumer's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stored {
    pub(crate) other: OrgSlot,
    pub(crate) node: NodeId,
    pub(crate) kind: EdgeKind,
    pub(crate) meta: EdgeMeta,
    /// The consumer entry (whose link set holds the edge).
    pub(crate) consumer: OrgSlot,
    /// The endpoint this is stored at is the consumer's node (else a
    /// dependency's default node).
    pub(crate) at_consumer: bool,
}

impl Stored {
    pub(crate) fn as_ref(&self) -> LinkRef {
        LinkRef {
            repo: self.other.repo,
            node: self.node,
            kind: self.kind,
            meta: self.meta,
            evidence_repo: self.consumer.repo,
            slot: self.other,
            evidence: self.consumer,
        }
    }
}

/// Every link edge of a generation as stored: each consumer entry's (a
/// repository's default or one of its refs) edges into its dependencies'
/// default entries, by endpoint. Views that read other entries remap them
/// ([`super::OrgIndex::link_edges`]).
#[derive(Debug, Default)]
pub struct LinkTable {
    out: HashMap<(OrgSlot, NodeId), Vec<Stored>>,
    inc: HashMap<(OrgSlot, NodeId), Vec<Stored>>,
    /// Per repository, the nodes of its default entry that link edges point
    /// at (what a ref of it must remap).
    targets: HashMap<usize, HashSet<NodeId>>,
    edges: usize,
}

impl LinkTable {
    pub(crate) fn load(dir: &Path, repos: &[OrgRepo]) -> Result<LinkTable> {
        let mut t = LinkTable::default();
        let index: HashMap<&str, usize> = repos.iter().enumerate().map(|(i, r)| (r.repo.as_str(), i)).collect();
        for (ci, r) in repos.iter().enumerate() {
            for (ref_idx, entry) in r.slots() {
                if let Some(links) = &entry.links {
                    t.add_link_set(dir, repos, &index, OrgSlot { repo: ci, ref_idx }, links)?;
                }
            }
        }
        for v in t.out.values_mut().chain(t.inc.values_mut()) {
            v.sort_by_key(|a| (a.kind, a.other, a.node));
        }
        Ok(t)
    }

    /// Reads and stores the link set `links` of consumer entry `consumer`.
    fn add_link_set(
        &mut self,
        dir: &Path,
        repos: &[OrgRepo],
        index: &HashMap<&str, usize>,
        consumer: OrgSlot,
        links: &OrgLinks,
    ) -> Result<()> {
        let Some(file) = &links.file else { return Ok(()) };
        // Dependencies that are not (or no longer at that segment) in the
        // generation cannot be read: their edges are dropped.
        let deps: Vec<Option<usize>> = links
            .deps
            .iter()
            .map(|d| index.get(d.repo.as_str()).copied().filter(|&j| repos[j].default.segment_id == d.segment_id))
            .collect();
        for e in read_links(dir, file)? {
            if let Some(Some(di)) = deps.get(e.dep as usize).copied() {
                self.add(consumer, OrgSlot::default_of(di), &e);
            }
        }
        Ok(())
    }

    /// Stores edge `e` of consumer entry `consumer` into dependency entry
    /// `dep` at both endpoints.
    fn add(&mut self, consumer: OrgSlot, dep: OrgSlot, e: &LinkEdge) {
        let c = (consumer, e.src);
        let d = (dep, e.dst);
        let (from, to) = if e.reversed { (d, c) } else { (c, d) };
        let stored = |other: (OrgSlot, NodeId), at_consumer: bool| Stored {
            other: other.0,
            node: other.1,
            kind: e.kind,
            meta: e.meta,
            consumer,
            at_consumer,
        };
        self.out.entry(from).or_default().push(stored(to, !e.reversed));
        self.inc.entry(to).or_default().push(stored(from, e.reversed));
        self.targets.entry(dep.repo).or_default().insert(e.dst);
        self.edges += 1;
    }

    /// The stored edges at `(slot, node)` in direction `dir`, optionally of
    /// one kind.
    pub(crate) fn stored(
        &self,
        slot: OrgSlot,
        node: NodeId,
        dir: LinkDir,
        kind: Option<EdgeKind>,
    ) -> impl Iterator<Item = &Stored> + '_ {
        let map = match dir {
            LinkDir::Out => &self.out,
            LinkDir::In => &self.inc,
        };
        map.get(&(slot, node)).into_iter().flatten().filter(move |e| kind.is_none_or(|k| e.kind == k))
    }

    /// The nodes of repository `repo`'s default entry that link edges point
    /// at.
    pub(crate) fn targets(&self, repo: usize) -> Option<&HashSet<NodeId>> {
        self.targets.get(&repo)
    }

    /// Link edges as stored at `(slot, node)` in direction `dir`, optionally
    /// of one kind, ordered by (kind, entry, node): a consumer entry's own
    /// edges and, at a default entry, the edges of consumers pointing at it
    /// (every consumer entry, whatever a view selects; see
    /// [`super::OrgIndex::link_edges`] for a view).
    pub fn edges(&self, slot: OrgSlot, node: NodeId, dir: LinkDir, kind: Option<EdgeKind>) -> Vec<LinkRef> {
        self.stored(slot, node, dir, kind).map(Stored::as_ref).collect()
    }

    /// Number of link edges.
    pub fn len(&self) -> usize {
        self.edges
    }

    pub fn is_empty(&self) -> bool {
        self.edges == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_on_separators() {
        assert!(import_matches("go:github.com/a/lib", "go:github.com/a/lib"));
        assert!(import_matches("go:github.com/a/lib", "go:github.com/a/lib/sub"));
        assert!(!import_matches("go:github.com/a/lib", "go:github.com/a/library"));
        assert!(import_matches("php:Carbon\\", "php:Carbon\\Carbon"));
        assert!(import_matches("php:Carbon", "php:Carbon\\Traits\\Date"));
        assert!(!import_matches("php:Carbon", "php:CarbonX\\A"));
        assert!(import_matches("py:click", "py:click.core"));
        assert!(!import_matches("py:click", "go:click"));
        assert!(import_matches("npm:@vue/runtime-core", "npm:@vue/runtime-core"));
        assert!(!import_matches("npm:vue", "npm:vuex"));
        let deps = RepoDeps {
            imports: vec!["go:github.com/x/y".into(), "py:click.core".into(), "py:jinja2".into()],
            requires: vec!["composer:nesbot/carbon".into()],
            ..Default::default()
        };
        let pkg = |eco: &str, name: &str, key: &str| Package {
            eco: eco.into(),
            name: name.into(),
            dir: String::new(),
            keys: vec![key.into()],
        };
        assert!(deps.depends_on_packages(&[pkg("pypi", "click", "py:click")]));
        assert!(deps.depends_on_packages(&[pkg("pypi", "Jinja2", "py:jinja2")]));
        assert!(!deps.depends_on_packages(&[pkg("pypi", "flask", "py:flask")]));
        assert!(deps.depends_on_packages(&[pkg("composer", "nesbot/Carbon", "php:Carbon")]));
        let other = RepoDeps { packages: vec![pkg("pypi", "click", "py:click")], ..Default::default() };
        assert!(deps.depends_on(&other));
        assert_eq!(normalize_package("pypi", "Foo_Bar.baz"), "foo-bar-baz");
    }

    fn ck(side: ContractSide, kind: ContractKind, key: &str, spec: bool) -> ContractKey {
        ContractKey { side, kind, key: key.into(), spec }
    }

    #[test]
    fn contracts_make_dependencies() {
        use ContractKind::{Http, Rpc};
        use ContractSide::{Consumes, Provides};
        let deps = |keys: Vec<ContractKey>| RepoDeps { contracts: keys, ..Default::default() };
        let server = deps(vec![ck(Provides, Http, "GET /users/{}", false)]);
        let spec = deps(vec![ck(Provides, Http, "GET /users/{}", true), ck(Provides, Rpc, "pkg.Users/Get", true)]);
        let client = deps(vec![ck(Consumes, Http, "GET /users/42", false)]);
        let stub = deps(vec![ck(Consumes, Rpc, "Users/Get", false)]);
        let other_route = deps(vec![ck(Consumes, Http, "POST /users/42", false)]);
        // Consumers depend on code and spec providers.
        assert!(client.depends_on(&server) && client.depends_on(&spec) && stub.depends_on(&spec));
        assert!(!other_route.depends_on(&server) && !stub.depends_on(&server));
        // A server depends on the spec it implements, not the reverse; two
        // servers or two specs never depend on each other.
        assert!(server.depends_on(&spec) && !spec.depends_on(&server));
        assert!(!server.depends_on(&server.clone()) && !spec.depends_on(&spec.clone()));
        // Providers never depend on consumers.
        assert!(!server.depends_on(&client));
        // Keys of a contract table: distinct, sorted, spec flagged.
        let row = |side, key: &str, framework: &str| ContractRow {
            node: 0,
            side,
            kind: Http,
            key: key.into(),
            raw: key.into(),
            file: 0,
            span: Default::default(),
            framework: framework.into(),
        };
        let keys = ContractKey::of_rows(&[
            row(Consumes, "GET /a", "requests"),
            row(Provides, "GET /a", "openapi"),
            row(Consumes, "GET /a", "fetch"),
            row(Provides, "GET /a", "flask"),
        ]);
        assert_eq!(
            keys,
            [
                ck(Provides, Http, "GET /a", false),
                ck(Provides, Http, "GET /a", true),
                ck(Consumes, Http, "GET /a", false)
            ]
        );
        // `contracts` defaults to empty in older profiles.
        let old: RepoDeps = serde_json::from_str(r#"{"packages": [], "imports": ["py:x"]}"#).unwrap();
        assert!(old.contracts.is_empty());
    }
}
