//! Which segments of a generation a query reads (RFC 0006 §9.3).
//!
//! Every repository of a generation has a **default entry** (its member
//! root) and zero or more **ref entries** (`OrgRepo::refs`, snapshots of
//! other branches, pull requests, tags or commits). An [`OrgSlot`] names one
//! of them; an [`OrgSelection`] says which slots of each repository a query
//! reads — the default unless the query names a ref — so that
//! `repo: A, ref: pr/123` is "the organization with A replaced by A at
//! `pr/123`" (§4) and `ref` without `repo` is the organization as of that
//! ref.
//!
//! Link sets point into their dependencies' **default** entries; a view that
//! selects a ref of a dependency sees those edges **remapped** to the ref's
//! nodes by (label, qualified name, path), and drops the ones whose target
//! the ref no longer has ([`super::OrgIndex::link_edges`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use rueca_model::{Label, NodeId, SnapshotInfo};

use super::{OrgEntry, OrgGeneration};
use crate::{Result, Segment, SegmentError};

/// Shortest commit prefix that names a snapshot.
const MIN_COMMIT_PREFIX: usize = 7;

/// One segment of a generation: repository `repo` (index into
/// [`OrgGeneration::repos`]) at its default entry (`ref_idx: None`) or at
/// `repos[repo].refs[ref_idx]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OrgSlot {
    pub repo: usize,
    pub ref_idx: Option<usize>,
}

impl OrgSlot {
    /// The default entry of repository `repo`.
    pub fn default_of(repo: usize) -> OrgSlot {
        OrgSlot { repo, ref_idx: None }
    }

    /// Whether this is a repository's default entry.
    pub fn is_default(&self) -> bool {
        self.ref_idx.is_none()
    }
}

/// One indexed ref of a repository, as [`OrgGeneration::refs_of`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRefInfo {
    pub slot: OrgSlot,
    /// The ref name: a ref entry's `ref_name`; for the default entry, the
    /// branch checked out at the member root (`HEAD` when detached,
    /// `worktree` outside git).
    pub ref_name: String,
    /// The snapshot's commit (full form as recorded).
    pub commit: String,
}

/// The ref name of a snapshot (RFC 0005 §4): its branch, `HEAD` when
/// detached, `worktree` outside git.
pub fn snapshot_ref_name(s: &SnapshotInfo) -> String {
    match s.branch.as_deref() {
        Some(b) if !b.is_empty() => b.to_string(),
        _ if s.commit.len() >= MIN_COMMIT_PREFIX && s.commit.bytes().all(|b| b.is_ascii_hexdigit()) => "HEAD".into(),
        _ => "worktree".into(),
    }
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether ref name `given` names the ref recorded as `recorded`: equal, or
/// both commits (hex) with one a prefix of at least 7 digits of the other
/// (a commit is recorded under its 12-digit form).
pub fn ref_names_match(recorded: &str, given: &str) -> bool {
    if recorded == given {
        return true;
    }
    let (r, g) = (recorded.to_ascii_lowercase(), given.to_ascii_lowercase());
    is_hex(&r) && is_hex(&g) && r.len().min(g.len()) >= MIN_COMMIT_PREFIX && (r.starts_with(&g) || g.starts_with(&r))
}

/// Whether `given` (≥ 7 hex digits, optionally with a trailing `+`) is a
/// prefix of `commit`.
fn names_commit(commit: &str, given: &str) -> bool {
    let g = given.trim_end_matches('+').to_ascii_lowercase();
    g.len() >= MIN_COMMIT_PREFIX && is_hex(&g) && commit.to_ascii_lowercase().starts_with(&g)
}

impl OrgGeneration {
    /// The entry of `slot`, if it exists.
    pub fn entry(&self, slot: OrgSlot) -> Option<&OrgEntry> {
        self.repos.get(slot.repo)?.entry(slot.ref_idx)
    }

    /// Every slot of the generation: repository by repository, the default
    /// entry first, then the refs in order.
    pub fn slots(&self) -> Vec<OrgSlot> {
        let mut out = Vec::new();
        for (i, r) in self.repos.iter().enumerate() {
            out.push(OrgSlot::default_of(i));
            out.extend((0..r.refs.len()).map(|k| OrgSlot { repo: i, ref_idx: Some(k) }));
        }
        out
    }

    /// The slot whose store segment is `segment_id` (the default entry when
    /// a default and a ref share it).
    pub fn slot_of_segment(&self, segment_id: &str) -> Option<OrgSlot> {
        self.slots().into_iter().find(|&s| self.entry(s).is_some_and(|e| e.segment_id == segment_id))
    }

    /// The ref name of `slot` ([`OrgRefInfo::ref_name`]).
    pub fn ref_name(&self, slot: OrgSlot) -> Option<String> {
        let r = self.repos.get(slot.repo)?;
        match slot.ref_idx {
            None => Some(snapshot_ref_name(&r.default.snapshot)),
            Some(k) => r.refs.get(k).map(|x| x.ref_name.clone()),
        }
    }

    /// Every indexed ref of repository `repo`: the default entry first, then
    /// the ref entries (empty when `repo` is out of range).
    pub fn refs_of(&self, repo: usize) -> Vec<OrgRefInfo> {
        let Some(r) = self.repos.get(repo) else { return Vec::new() };
        let mut out = vec![OrgRefInfo {
            slot: OrgSlot::default_of(repo),
            ref_name: snapshot_ref_name(&r.default.snapshot),
            commit: r.default.snapshot.commit.clone(),
        }];
        out.extend(r.refs.iter().enumerate().map(|(k, x)| OrgRefInfo {
            slot: OrgSlot { repo, ref_idx: Some(k) },
            ref_name: x.ref_name.clone(),
            commit: x.entry.snapshot.commit.clone(),
        }));
        out
    }

    /// The slot of repository `repo` that ref `name` names: a ref entry with
    /// that name (a commit by any prefix of at least 7 digits), else the
    /// default entry when it is the branch checked out at the member root,
    /// else the entry whose commit starts with `name` (≥ 7 hex digits). A ref
    /// entry wins over the default when both carry the name (`--ref main`
    /// exports the commit `main` points to; the default may be dirty).
    pub fn find_ref(&self, repo: usize, name: &str) -> Option<OrgSlot> {
        let r = self.repos.get(repo)?;
        if let Some(k) = r.refs.iter().position(|x| ref_names_match(&x.ref_name, name)) {
            return Some(OrgSlot { repo, ref_idx: Some(k) });
        }
        if snapshot_ref_name(&r.default.snapshot) == name {
            return Some(OrgSlot::default_of(repo));
        }
        self.refs_of(repo).into_iter().find(|x| names_commit(&x.commit, name)).map(|x| x.slot)
    }

    /// The slots of repository `repo` that `names` name, in order, each
    /// once; an error naming the indexed refs when one is not indexed.
    pub fn select(&self, repo: usize, names: &[&str]) -> Result<Vec<OrgSlot>> {
        let r = self.repos.get(repo).ok_or_else(|| SegmentError::NotFound(format!("repository #{repo}")))?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let slot = self.find_ref(repo, name).ok_or_else(|| {
                SegmentError::NotFound(format!(
                    "ref `{name}` is not indexed for repository `{}` in organization `{}`; indexed refs: {}",
                    r.repo,
                    self.name,
                    self.indexed_refs(repo)
                ))
            })?;
            if !out.contains(&slot) {
                out.push(slot);
            }
        }
        Ok(out)
    }

    /// `main@0123456789ab, pr/7@…` for error messages.
    pub fn indexed_refs(&self, repo: usize) -> String {
        self.refs_of(repo)
            .iter()
            .map(|x| format!("{}@{}", x.ref_name, &x.commit[..x.commit.len().min(12)]))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Which slots of each repository a query reads: every repository at its
/// default entry unless [`OrgSelection::set`] chose others (one ref, or
/// several for a multi-ref comparison). The empty selection is the
/// organization at its default refs (phase 1 and 2 behaviour).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgSelection {
    chosen: BTreeMap<usize, Vec<OrgSlot>>,
}

impl OrgSelection {
    /// Reads repository `repo` at `slots` (those of another repository are
    /// ignored; duplicates dropped); an empty list restores the default.
    pub fn set(&mut self, repo: usize, slots: Vec<OrgSlot>) {
        let mut mine: Vec<OrgSlot> = Vec::with_capacity(slots.len());
        for s in slots.into_iter().filter(|s| s.repo == repo) {
            if !mine.contains(&s) {
                mine.push(s);
            }
        }
        if mine.is_empty() {
            self.chosen.remove(&repo);
        } else {
            self.chosen.insert(repo, mine);
        }
    }

    /// The slots read for repository `repo`, in the order chosen (its
    /// default entry when nothing was chosen).
    pub fn slots_of(&self, repo: usize) -> Vec<OrgSlot> {
        self.chosen.get(&repo).cloned().unwrap_or_else(|| vec![OrgSlot::default_of(repo)])
    }

    /// The first slot read for repository `repo`: the one routing scores
    /// it with.
    pub fn primary(&self, repo: usize) -> OrgSlot {
        self.chosen.get(&repo).and_then(|v| v.first().copied()).unwrap_or(OrgSlot::default_of(repo))
    }

    /// Whether `slot` is read.
    pub fn contains(&self, slot: OrgSlot) -> bool {
        match self.chosen.get(&slot.repo) {
            Some(v) => v.contains(&slot),
            None => slot.is_default(),
        }
    }

    /// Whether every repository is read at its default entry.
    pub fn is_default(&self) -> bool {
        self.chosen.values().all(|v| v.len() == 1 && v[0].is_default())
    }

    /// Repositories with an explicit choice.
    pub fn chosen_repos(&self) -> impl Iterator<Item = usize> + '_ {
        self.chosen.keys().copied()
    }
}

/// Node correspondence between a repository's default entry and one of its
/// ref entries, for the default nodes link edges point at: built once per
/// (repository, ref) by an opened [`super::OrgIndex`].
#[derive(Debug, Default)]
pub(crate) struct Remap {
    /// Default node → the ref's node with the same (label, qualified name,
    /// path); absent when the ref no longer has it.
    pub(crate) to_ref: HashMap<NodeId, NodeId>,
    /// The inverse.
    pub(crate) to_default: HashMap<NodeId, NodeId>,
}

/// (label, qualified name, path of its file).
type NodeKey<'a> = (Label, &'a str, Option<&'a str>);

fn node_key(seg: &Segment, n: NodeId) -> NodeKey<'_> {
    let node = seg.node(n);
    let path = node.file.and_then(|f| seg.files().get(f as usize)).map(|f| f.path.as_str());
    (node.label, node.qualified_name.as_str(), path)
}

/// Source order of node `n` among nodes with one key: start byte, then id.
fn source_order(seg: &Segment, n: NodeId) -> (u32, NodeId) {
    (seg.node(n).span.map_or(u32::MAX, |s| s.start_byte), n)
}

/// The nodes of `seg` whose key is one of `keys`, grouped by key in source
/// order.
fn nodes_by_key<'a>(seg: &'a Segment, keys: &HashSet<NodeKey<'_>>) -> HashMap<NodeKey<'a>, Vec<NodeId>> {
    let mut out: HashMap<NodeKey<'a>, Vec<NodeId>> = HashMap::with_capacity(keys.len());
    for n in 0..seg.nodes().len() as NodeId {
        let key = node_key(seg, n);
        if keys.contains(&key) {
            out.entry(key).or_default().push(n);
        }
    }
    for nodes in out.values_mut() {
        nodes.sort_unstable_by_key(|&n| source_order(seg, n));
    }
    out
}

impl Remap {
    /// Maps `targets` (nodes of `default`) to the nodes of `at_ref` with
    /// the same key. Several nodes with one key (a property's getter and
    /// setter, overloads) pair by position in source order; a node past
    /// the other side's count has no counterpart.
    pub(crate) fn build(default: &Segment, at_ref: &Segment, targets: &HashSet<NodeId>) -> Remap {
        let mut out = Remap::default();
        let n_default = default.nodes().len();
        let keys: HashSet<NodeKey<'_>> =
            targets.iter().filter(|&&n| (n as usize) < n_default).map(|&n| node_key(default, n)).collect();
        if keys.is_empty() {
            return out;
        }
        let ours = nodes_by_key(default, &keys);
        let theirs = nodes_by_key(at_ref, &keys);
        for (key, nodes) in &ours {
            let Some(mapped) = theirs.get(key) else { continue };
            for (&d, &r) in nodes.iter().zip(mapped).filter(|(d, _)| targets.contains(d)) {
                out.to_ref.insert(d, r);
                out.to_default.insert(r, d);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_selection() {
        assert!(ref_names_match("feature/x", "feature/x"));
        assert!(ref_names_match("7696364aef89", "7696364"));
        assert!(!ref_names_match("7696364aef89", "769636"));
        assert!(!ref_names_match("feature/x", "feature"));
        assert!(names_commit("7696364aef89a22e", "7696364+"));
        assert!(!names_commit("7696364aef89a22e", "main"));
        let mut s = OrgSelection::default();
        assert!(s.is_default() && s.contains(OrgSlot::default_of(3)));
        let r = OrgSlot { repo: 1, ref_idx: Some(0) };
        s.set(1, vec![r, r, OrgSlot::default_of(2)]);
        assert_eq!(s.slots_of(1), [r]);
        assert_eq!(s.primary(1), r);
        assert!(!s.contains(OrgSlot::default_of(1)) && s.contains(r) && !s.is_default());
        assert_eq!(s.slots_of(0), [OrgSlot::default_of(0)]);
        s.set(1, Vec::new());
        assert!(s.is_default());
    }

    struct Src(String);

    impl crate::ContentSource for Src {
        fn content(&self, _: &rueca_model::FileEntry) -> std::io::Result<Vec<u8>> {
            Ok(self.0.clone().into_bytes())
        }
    }

    /// A segment of one Python file `m.py` whose `def <name>(` lines are
    /// functions `m.<name>` (a property's getter and setter share a name).
    fn segment(store: &crate::Store, commit: &str, src: &str) -> Segment {
        use rueca_model::{FileEntry, Language, Node, ResolvedGraph, Span};
        let node = |label, name: &str, qn: &str, span| Node {
            label,
            name: name.into(),
            qualified_name: qn.into(),
            file: Some(0),
            span,
            language: Some(Language::Python),
            signature: None,
            doc: None,
            exported: true,
            is_test: false,
        };
        let mut nodes = vec![node(Label::File, "m.py", "m.py", None)];
        let mut at = 0;
        for (line, text) in src.lines().enumerate() {
            if let Some(name) = text.strip_prefix("def ").and_then(|r| r.split('(').next()) {
                let (b, l) = (at as u32, line as u32);
                let span = Span {
                    start_byte: b,
                    end_byte: b + text.len() as u32,
                    start_line: l,
                    start_col: 0,
                    end_line: l,
                    end_col: 0,
                };
                nodes.push(node(Label::Function, name, &format!("m.{name}"), Some(span)));
            }
            at += text.len() + 1;
        }
        let snapshot = SnapshotInfo {
            repo: "r".into(),
            commit: commit.into(),
            dirty: false,
            branch: None,
            root: None,
            indexed_at: 0,
            rueca_version: "test".into(),
        };
        let files = vec![FileEntry {
            path: "m.py".into(),
            blob_sha: rueca_model::git_blob_sha(src.as_bytes()),
            language: Some(Language::Python),
            size: src.len() as u64,
            line_count: src.lines().count() as u32,
            node: 0,
            status: "ok".into(),
        }];
        let g = ResolvedGraph { snapshot, nodes, edges: vec![], files, modules: vec![] };
        let meta = crate::write_segment(&g, &Src(src.into()), &store.dir).unwrap();
        store.open_segment(&meta.id).unwrap()
    }

    #[test]
    fn same_named_nodes_pair_in_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path()).unwrap();
        let default = segment(&store, "c1", "def prop(self):\ndef prop(self, v):\n");
        // A function before them shifts every id.
        let at_ref = segment(&store, "c2", "def first():\ndef prop(self):\ndef prop(self, v):\n");
        let (get, set) = (1, 2);
        let m = Remap::build(&default, &at_ref, &HashSet::from([get, set]));
        assert_eq!((m.to_ref.get(&get), m.to_ref.get(&set)), (Some(&2), Some(&3)));
        assert_eq!((m.to_default.get(&2), m.to_default.get(&3)), (Some(&get), Some(&set)));
        // Only the setter targeted: still paired with the ref's second.
        let m = Remap::build(&default, &at_ref, &HashSet::from([set]));
        assert_eq!((m.to_ref.len(), m.to_ref.get(&set)), (1, Some(&3)));
        // The ref has one: the second has no counterpart.
        let one = segment(&store, "c3", "def prop(self):\n");
        let m = Remap::build(&default, &one, &HashSet::from([get, set]));
        assert_eq!((m.to_ref.get(&get), m.to_ref.get(&set)), (Some(&1), None));
    }
}
