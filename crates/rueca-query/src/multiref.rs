//! Collapse of results read from several refs of one repository (RFC 0005 §6).
//!
//! Extraction is per blob, so a symbol whose file blob is the same in two
//! refs has identical facts. When a response reads several snapshots of a
//! repository (a branch and a pull request), symbols with the same
//! `(repository, path, qualified name, blob)` become **one item** with the
//! list of snapshots they appear `in`; symbols that differ appear once per
//! distinct version, and every version of a symbol that is not identical in
//! all requested snapshots of its repository is marked `changed` (a
//! different blob, or absent from some snapshot: added or deleted on that
//! ref). `trace` edges collapse the same way, by `(repository, source,
//! destination, kind)`.
//!
//! Local mode keeps one snapshot per root, so no tool reaches this yet; it
//! is the function the service and org modes apply to merged results. The
//! item kept for a group is the first one seen, so its id is pinned to that
//! snapshot and stays valid on its own.

use std::collections::{BTreeSet, HashMap};

/// One item as read from snapshot `s`.
#[derive(Debug, Clone, PartialEq)]
pub struct RefItem<T> {
    /// Index of the snapshot in the response's `snapshots`.
    pub s: usize,
    pub repo: String,
    pub path: Option<String>,
    /// Global qualified name.
    pub qn: String,
    /// Git blob id of the defining file (`None`: no file, e.g. canonical env vars).
    pub blob: Option<String>,
    pub item: T,
}

/// One edge as read from snapshot `s`; endpoints are `(path, qualified name)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RefEdge<T> {
    pub s: usize,
    pub repo: String,
    pub src: (Option<String>, String),
    pub dst: (Option<String>, String),
    pub kind: String,
    pub item: T,
}

/// A collapsed item: the first occurrence, the snapshots it appears in, and
/// whether it differs across the requested snapshots of its repository.
#[derive(Debug, Clone, PartialEq)]
pub struct Collapsed<T> {
    pub item: T,
    /// Snapshot indexes, ascending.
    pub in_: Vec<usize>,
    pub changed: bool,
}

/// Collapses `items` read from `snapshot_repos` (the repository of each
/// snapshot index). Output order: first appearance.
pub fn collapse<T>(snapshot_repos: &[&str], items: Vec<RefItem<T>>) -> Vec<Collapsed<T>> {
    let keyed = items.into_iter().map(|i| {
        let version = (i.repo.clone(), i.path.clone(), i.qn.clone(), i.blob.clone());
        let symbol = (i.repo.clone(), i.path, i.qn);
        (i.s, i.repo, version, symbol, i.item)
    });
    collapse_keyed(snapshot_repos, keyed)
}

/// Collapses `edges` like [`collapse`]: the same edge in several snapshots is
/// one item; an edge missing from some requested snapshot of its repository is
/// `changed`.
pub fn collapse_edges<T>(snapshot_repos: &[&str], edges: Vec<RefEdge<T>>) -> Vec<Collapsed<T>> {
    let keyed = edges.into_iter().map(|e| {
        let key = (e.repo.clone(), e.src, e.dst, e.kind);
        (e.s, e.repo, key.clone(), key, e.item)
    });
    collapse_keyed(snapshot_repos, keyed)
}

/// Shared core: `version` identifies identical items, `entity` groups the
/// versions of one thing (a symbol, an edge).
fn collapse_keyed<K: Clone + Eq + std::hash::Hash, E: Eq + std::hash::Hash, T>(
    snapshot_repos: &[&str],
    items: impl IntoIterator<Item = (usize, String, K, E, T)>,
) -> Vec<Collapsed<T>> {
    let mut out: Vec<(Collapsed<T>, usize, String)> = Vec::new();
    let mut by_version: HashMap<K, usize> = HashMap::new();
    let mut entities: HashMap<E, usize> = HashMap::new();
    let mut versions_of: Vec<Vec<usize>> = Vec::new();
    for (s, repo, version, entity, item) in items {
        if let Some(&i) = by_version.get(&version) {
            if !out[i].0.in_.contains(&s) {
                out[i].0.in_.push(s);
            }
            continue;
        }
        let e = *entities.entry(entity).or_insert_with(|| {
            versions_of.push(Vec::new());
            versions_of.len() - 1
        });
        by_version.insert(version, out.len());
        versions_of[e].push(out.len());
        out.push((Collapsed { item, in_: vec![s], changed: false }, e, repo));
    }
    for versions in &versions_of {
        let repo = out[versions[0]].2.clone();
        let requested: BTreeSet<usize> =
            snapshot_repos.iter().enumerate().filter(|(_, r)| **r == repo).map(|(i, _)| i).collect();
        let identical_everywhere = versions.len() == 1 && requested.iter().all(|s| out[versions[0]].0.in_.contains(s));
        for &v in versions {
            out[v].0.changed = !identical_everywhere;
        }
    }
    out.into_iter()
        .map(|(mut c, _, _)| {
            c.in_.sort_unstable();
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "github.com/acme/billing";

    fn item(s: usize, path: &str, qn: &str, blob: &str) -> RefItem<String> {
        RefItem {
            s,
            repo: REPO.into(),
            path: Some(path.into()),
            qn: qn.into(),
            blob: Some(blob.into()),
            item: format!("{qn}@{s}"),
        }
    }

    /// Two snapshots of one repository (`main` = 0, `pr/7` = 1) sharing the
    /// blob of `a.py`, with `b.py` changed and `c.py` added on the PR, plus a
    /// third snapshot of another repository.
    #[test]
    fn identical_symbols_collapse_and_changes_are_flagged() {
        let repos = [REPO, REPO, "github.com/acme/web"];
        let items = vec![
            item(0, "a.py", "a.f", "blob-a"),
            item(0, "b.py", "b.g", "blob-b1"),
            item(1, "a.py", "a.f", "blob-a"),
            item(1, "b.py", "b.g", "blob-b2"),
            item(1, "c.py", "c.h", "blob-c"),
            item(0, "a.py", "a.f", "blob-a"), // duplicate within a snapshot
            RefItem {
                s: 2,
                repo: "github.com/acme/web".into(),
                path: Some("a.py".into()),
                qn: "a.f".into(),
                blob: Some("blob-a".into()),
                item: "web a.f".into(),
            },
        ];
        let c = collapse(&repos, items);
        let got: Vec<(&str, Vec<usize>, bool)> =
            c.iter().map(|c| (c.item.as_str(), c.in_.clone(), c.changed)).collect();
        assert_eq!(
            got,
            vec![
                ("a.f@0", vec![0, 1], false),
                ("b.g@0", vec![0], true),
                ("b.g@1", vec![1], true),
                ("c.h@1", vec![1], true),
                ("web a.f", vec![2], false),
            ]
        );
    }

    #[test]
    fn deleted_symbols_are_changed() {
        let repos = [REPO, REPO];
        let c = collapse(&repos, vec![item(0, "old.py", "old.f", "x")]);
        assert_eq!((c[0].in_.clone(), c[0].changed), (vec![0], true));
        // A single requested snapshot never reports changes.
        let c = collapse(&[REPO], vec![item(0, "old.py", "old.f", "x")]);
        assert!(!c[0].changed);
    }

    #[test]
    fn edges_union_with_their_snapshots() {
        let repos = [REPO, REPO];
        let edge = |s: usize, src: &str, dst: &str| RefEdge {
            s,
            repo: REPO.into(),
            src: (Some("a.py".into()), src.into()),
            dst: (Some("b.py".into()), dst.into()),
            kind: "CALLS".into(),
            item: (s, src.to_string(), dst.to_string()),
        };
        let c = collapse_edges(&repos, vec![edge(0, "a.f", "b.g"), edge(1, "a.f", "b.g"), edge(1, "a.f", "b.new")]);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].in_.clone(), c[0].changed), (vec![0, 1], false));
        // Who calls `b.new` on the PR but not on main: in [1], changed.
        assert_eq!((c[1].in_.clone(), c[1].changed, c[1].item.2.as_str()), (vec![1], true, "b.new"));
    }
}
