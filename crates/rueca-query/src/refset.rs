//! Responses that read several snapshots of one repository (RFC 0005 §6,
//! RFC 0006 §9.3): `ref: ["main", "pr/7"]`, or two working trees of one
//! repository. The tools find the same symbol in every snapshot of its
//! repository — same label, qualified name and file path
//! ([`View::same_symbol`]) — and collapse identical versions (same file
//! blob) with [`crate::multiref`]: one item with the snapshots it is `in`,
//! `changed` when it is not identical in every requested snapshot of its
//! repository. Repositories read at one snapshot get no mark, so their items
//! are exactly the single-snapshot ones.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rueca_model::{EdgeKind, EdgeMeta, FileId, Label, NodeId};

use crate::multiref::{Collapsed, RefEdge, RefItem, collapse, collapse_edges};
use crate::types::RefMark;
use crate::view::View;

/// A node of a call: (view index, node).
pub(crate) type Key = (usize, NodeId);

/// What identifies a symbol across snapshots: (repository, path, qualified
/// name, label).
pub(crate) type Ident = (String, Option<String>, String, Label);

/// A traversed edge: (src, dst, kind) → (strongest metadata, view whose file
/// table its evidence names).
pub(crate) type EdgeMap = BTreeMap<(Key, Key, EdgeKind), (EdgeMeta, usize)>;

/// An edge of a walk: (src, dst, kind, (metadata, evidence view), mark).
pub(crate) type TracedEdge = (Key, Key, EdgeKind, (EdgeMeta, usize), Option<RefMark>);

/// The views of the repository of view `vi`, ascending (`vi` included).
pub(crate) fn peers(views: &[View], vi: usize) -> Vec<usize> {
    let repo = views[vi].repo();
    (0..views.len()).filter(|&j| views[j].repo() == repo).collect()
}

/// Whether some repository is read at several snapshots.
pub(crate) fn any_multi(views: &[View]) -> bool {
    max_per_repo(views) > 1
}

/// The largest number of snapshots read of one repository.
pub(crate) fn max_per_repo(views: &[View]) -> usize {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for v in views {
        *count.entry(v.repo()).or_default() += 1;
    }
    count.into_values().max().unwrap_or(0)
}

pub(crate) fn ident(views: &[View], k: Key) -> Ident {
    let v = &views[k.0];
    let node = v.node(k.1);
    (v.repo().to_string(), v.node_path(k.1).map(str::to_string), node.qualified_name.clone(), node.label)
}

fn repos_of(views: &[View]) -> Vec<&str> {
    views.iter().map(View::repo).collect()
}

/// A [`RefItem`] for node `n` of view `vi`. The key's qualified name carries
/// the start byte, so two definitions of one name in one file stay apart
/// (identical blobs have identical spans).
fn ref_item<T>(views: &[View], vi: usize, n: NodeId, item: T) -> RefItem<T> {
    let v = &views[vi];
    let node = v.node(n);
    let start = node.span.map_or(0, |s| s.start_byte);
    RefItem {
        s: vi,
        repo: v.repo().to_string(),
        path: v.node_path(n).map(str::to_string),
        qn: format!("{}\u{0}{start}", node.qualified_name),
        blob: v.blob_of(n).map(str::to_string),
        item,
    }
}

/// Node `k` and the same symbol in every other snapshot of its repository
/// that has it (view order): the roots of a multi-ref `trace`.
pub(crate) fn versions_of(views: &[View], k: Key) -> Vec<Key> {
    let others = peers(views, k.0).into_iter().filter(|&p| p != k.0);
    let found = others.filter_map(|p| views[p].same_symbol(&views[k.0], k.1).map(|m| (p, m)));
    std::iter::once(k).chain(found).collect()
}

/// The mark of node `n` of view `vi`, looked up in every other snapshot of
/// its repository; `None` when the repository is read at one snapshot.
pub(crate) fn mark_of(views: &[View], vi: usize, n: NodeId) -> Option<RefMark> {
    let peers = peers(views, vi);
    if peers.len() < 2 {
        return None;
    }
    let blob = views[vi].blob_of(n);
    let in_: Vec<usize> = peers
        .iter()
        .copied()
        .filter(|&p| {
            let same = if p == vi { Some(n) } else { views[p].same_symbol(&views[vi], n) };
            same.is_some_and(|m| views[p].blob_of(m) == blob)
        })
        .collect();
    let changed = in_.len() != peers.len();
    Some(RefMark { in_, changed, peers })
}

/// Search hits `(view, node, score)` in rank order with identical versions
/// collapsed into their first (best-ranked) occurrence, each with its mark.
/// The marks are looked up rather than read off the hits, so a version whose
/// twin ranked past the fetched hits is still `in` both.
pub(crate) fn collapse_hits(views: &[View], ranked: Vec<(usize, NodeId, f64)>) -> Vec<(Key, f64, Option<RefMark>)> {
    let items: Vec<RefItem<(usize, NodeId, f64)>> =
        ranked.into_iter().map(|(vi, n, score)| ref_item(views, vi, n, (vi, n, score))).collect();
    collapse(&repos_of(views), items)
        .into_iter()
        .map(|c| {
            let (vi, n, score) = c.item;
            ((vi, n), score, mark_of(views, vi, n))
        })
        .collect()
}

/// The distinct versions of node `n` of view `vi` over the snapshots of its
/// repository, the version of `vi` first: (view, node, mark). One version
/// without a mark when the repository is read at one snapshot.
pub(crate) fn symbol_versions(views: &[View], vi: usize, n: NodeId) -> Vec<(Key, Option<RefMark>)> {
    let peers = peers(views, vi);
    if peers.len() < 2 {
        return vec![((vi, n), None)];
    }
    let mut items = vec![ref_item(views, vi, n, (vi, n))];
    for p in peers.into_iter().filter(|&p| p != vi) {
        if let Some(m) = views[p].same_symbol(&views[vi], n) {
            items.push(ref_item(views, p, m, (p, m)));
        }
    }
    collapse(&repos_of(views), items).into_iter().map(|c| (c.item, Some(mark(views, &c)))).collect()
}

/// The distinct versions of a file found at `hits` (view, file) in
/// snapshots of one repository, collapsed by blob, the first hit's version
/// first: (view, file, mark).
pub(crate) fn file_versions(views: &[View], hits: &[(usize, FileId)]) -> Vec<(usize, FileId, RefMark)> {
    let items: Vec<RefItem<(usize, FileId)>> = hits
        .iter()
        .map(|&(vi, f)| {
            let file = views[vi].seg.file(f);
            RefItem {
                s: vi,
                repo: views[vi].repo().to_string(),
                path: Some(file.path.clone()),
                qn: String::new(),
                blob: Some(file.blob_sha.clone()),
                item: (vi, f),
            }
        })
        .collect();
    collapse(&repos_of(views), items).into_iter().map(|c| (c.item.0, c.item.1, mark(views, &c))).collect()
}

/// The mark of collapsed item `c` (`in` non-empty).
fn mark<T>(views: &[View], c: &Collapsed<T>) -> RefMark {
    RefMark { in_: c.in_.clone(), changed: c.changed, peers: peers(views, c.in_[0]) }
}

/// The nodes of a multi-ref `trace` merged by symbol: every node key maps to
/// one representative (the root for the root's versions, else the
/// occurrence in the lowest view), and each representative of a repository
/// read at several snapshots has a mark: the snapshots it was reached in,
/// `changed` when that is not every snapshot of its repository or its
/// versions differ.
pub(crate) struct NodeMerge {
    pub rep: HashMap<Key, Key>,
    pub marks: HashMap<Key, RefMark>,
}

impl NodeMerge {
    /// `roots`: the root first, then its versions in other snapshots.
    pub(crate) fn build(views: &[View], roots: &[Key], keys: impl IntoIterator<Item = Key>) -> NodeMerge {
        let root = roots[0];
        let mut groups: BTreeMap<Ident, BTreeSet<Key>> = BTreeMap::new();
        for k in roots.iter().copied().chain(keys) {
            groups.entry(ident(views, k)).or_default().insert(k);
        }
        let repos = repos_of(views);
        let mut out = NodeMerge { rep: HashMap::new(), marks: HashMap::new() };
        for keys in groups.values() {
            let rep = if keys.contains(&root) { root } else { *keys.first().expect("non-empty group") };
            out.rep.extend(keys.iter().map(|&k| (k, rep)));
            if peers(views, rep.0).len() < 2 {
                continue;
            }
            let items: Vec<RefItem<()>> = keys.iter().map(|&(vi, n)| ref_item(views, vi, n, ())).collect();
            let versions = collapse(&repos, items);
            let in_: BTreeSet<usize> = versions.iter().flat_map(|c| c.in_.iter().copied()).collect();
            let changed = versions.iter().any(|c| c.changed);
            out.marks.insert(rep, RefMark { in_: in_.into_iter().collect(), changed, peers: peers(views, rep.0) });
        }
        out
    }

    pub(crate) fn of(&self, k: Key) -> Key {
        self.rep.get(&k).copied().unwrap_or(k)
    }

    /// `edges` with their endpoints replaced by representatives, the same
    /// edge of several snapshots collapsed into one (its first occurrence)
    /// with a mark when its repository is read at several snapshots: the
    /// snapshots whose walk found it. An edge's snapshot is that of its
    /// endpoint in a multi-ref repository (else its source's).
    pub(crate) fn edges(&self, views: &[View], edges: EdgeMap) -> Vec<TracedEdge> {
        let end = |k: Key| {
            let rep = self.of(k);
            (views[rep.0].node_path(rep.1).map(str::to_string), views[rep.0].node(rep.1).qualified_name.clone())
        };
        let list: Vec<RefEdge<TracedEdge>> = edges
            .into_iter()
            .map(|((src, dst, kind), meta)| {
                let s = if peers(views, src.0).len() > 1 { src.0 } else { dst.0 };
                RefEdge {
                    s,
                    repo: views[s].repo().to_string(),
                    src: end(src),
                    dst: end(dst),
                    kind: kind.as_str().to_string(),
                    item: (self.of(src), self.of(dst), kind, meta, None),
                }
            })
            .collect();
        let repos = repos_of(views);
        collapse_edges(&repos, list)
            .into_iter()
            .map(|c| {
                let multi = c.in_.first().is_some_and(|&s| peers(views, s).len() > 1);
                let m = multi.then(|| mark(views, &c));
                let (src, dst, kind, meta, _) = c.item;
                (src, dst, kind, meta, m)
            })
            .collect()
    }
}
