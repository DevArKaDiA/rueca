//! The organization link layer, packages (RFC 0006 §3.3.1): the storage side.
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

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use rueca_model::{EdgeKind, EdgeMeta, NodeId};
use serde::{Deserialize, Serialize};

use super::{OrgRepo, corrupt};
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
}

/// Separator after which an import key continues a package key.
fn key_separator(key: &str) -> &'static str {
    match key.split_once(':').map_or("", |(eco, _)| eco) {
        "py" | "java" => ".",
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
    /// Whether this repository depends on a repository publishing `published`.
    pub fn depends_on(&self, published: &[Package]) -> bool {
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

/// A cross-repository edge from a consumer node to a node of one of its
/// dependencies. `meta.evidence` names a file of the consumer.
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
}

/// One side of a link computation: a repository of the generation being
/// built, its opened segment and what it publishes / imports.
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
    /// The other endpoint: repository index in the generation and node.
    pub repo: usize,
    pub node: NodeId,
    pub kind: EdgeKind,
    pub meta: EdgeMeta,
    /// Repository whose file table `meta.evidence` refers to (the consumer).
    pub evidence_repo: usize,
}

/// Every link edge of a generation, by endpoint.
#[derive(Debug, Default)]
pub struct LinkTable {
    out: HashMap<(usize, NodeId), Vec<LinkRef>>,
    inc: HashMap<(usize, NodeId), Vec<LinkRef>>,
    edges: usize,
}

impl LinkTable {
    pub(crate) fn load(dir: &Path, repos: &[OrgRepo]) -> Result<LinkTable> {
        let mut t = LinkTable::default();
        let index: HashMap<&str, usize> = repos.iter().enumerate().map(|(i, r)| (r.repo.as_str(), i)).collect();
        for (ci, r) in repos.iter().enumerate() {
            let Some(links) = &r.links else { continue };
            let Some(file) = &links.file else { continue };
            // Dependencies that are not (or no longer at that segment) in
            // the generation cannot be read: their edges are dropped.
            let deps: Vec<Option<usize>> = links
                .deps
                .iter()
                .map(|d| index.get(d.repo.as_str()).copied().filter(|&j| repos[j].segment_id == d.segment_id))
                .collect();
            for e in read_links(dir, file)? {
                let Some(Some(di)) = deps.get(e.dep as usize).copied() else { continue };
                t.out.entry((ci, e.src)).or_default().push(LinkRef {
                    repo: di,
                    node: e.dst,
                    kind: e.kind,
                    meta: e.meta,
                    evidence_repo: ci,
                });
                t.inc.entry((di, e.dst)).or_default().push(LinkRef {
                    repo: ci,
                    node: e.src,
                    kind: e.kind,
                    meta: e.meta,
                    evidence_repo: ci,
                });
                t.edges += 1;
            }
        }
        for v in t.out.values_mut().chain(t.inc.values_mut()) {
            v.sort_by_key(|a| (a.kind, a.repo, a.node));
        }
        Ok(t)
    }

    /// Link edges at `(repo, node)` in direction `dir`, optionally of one
    /// kind, ordered by (kind, repository, node).
    pub fn edges(&self, repo: usize, node: NodeId, dir: LinkDir, kind: Option<EdgeKind>) -> Vec<LinkRef> {
        let map = match dir {
            LinkDir::Out => &self.out,
            LinkDir::In => &self.inc,
        };
        map.get(&(repo, node))
            .map(|v| v.iter().filter(|e| kind.is_none_or(|k| e.kind == k)).copied().collect())
            .unwrap_or_default()
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
        assert!(deps.depends_on(&[pkg("pypi", "click", "py:click")]));
        assert!(deps.depends_on(&[pkg("pypi", "Jinja2", "py:jinja2")]));
        assert!(!deps.depends_on(&[pkg("pypi", "flask", "py:flask")]));
        assert!(deps.depends_on(&[pkg("composer", "nesbot/Carbon", "php:Carbon")]));
        assert_eq!(normalize_package("pypi", "Foo_Bar.baz"), "foo-bar-baz");
    }
}
