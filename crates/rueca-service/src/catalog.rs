//! The per-repository catalog (RFC 0008 §5.3) and retention (§5.8).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Version of `catalog.json`.
pub const CATALOG_FORMAT: u32 = 1;

/// Entries kept in [`RepoCatalog::removed`] (newest last).
const REMOVED_KEPT: usize = 100;

/// `repos/<repo-dir>/catalog.json`: the refs of one repository and their
/// snapshots, written by compare-and-swap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCatalog {
    pub format: u32,
    /// Repository id (`github.com/acme/billing`).
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The ref of kind [`RefKind::Default`] (`main`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_ref: Option<String>,
    pub refs: BTreeMap<String, RefEntry>,
    /// Refs expired or removed, newest last (at most 100).
    #[serde(default)]
    pub removed: Vec<Removed>,
}

/// One ref of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefEntry {
    pub kind: RefKind,
    /// Newest first, at most the retention of the kind: the first is what
    /// queries read; the others stay readable by pinned id until collected.
    pub snapshots: Vec<SnapshotEntry>,
    /// When the pull request was closed or merged (unix seconds); set by
    /// the refs lifecycle (phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
}

/// What a ref is (RFC 0008 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    /// The repository's default branch.
    Default,
    Branch,
    Tag,
    /// `pr/<n>`.
    Pr,
    /// A commit or another ref indexed on demand.
    Commit,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Default => "default",
            RefKind::Branch => "branch",
            RefKind::Tag => "tag",
            RefKind::Pr => "pr",
            RefKind::Commit => "commit",
        }
    }
}

/// One published snapshot of a ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    /// Full commit.
    pub commit: String,
    /// Segment id (`repos/<repo-dir>/segments/<segment>.rcseg`).
    pub segment: String,
    /// Unix seconds.
    pub indexed_at: u64,
    pub rueca_version: String,
    /// Size of the segment file.
    pub bytes: u64,
    /// Pack ids (`repos/<repo-dir>/packs/<id>.rcpack`) holding the
    /// snapshot's blobs (`SegmentMeta::packs`).
    pub packs: Vec<String>,
    /// The vectors of the snapshot (phase 3, RFC 0007).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vectors: Option<VectorsEntry>,
    /// A working tree with uncommitted changes: never a duplicate of
    /// another snapshot of its commit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dirty: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The vector segment of a snapshot (RFC 0008 §7.11, phase 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorsEntry {
    pub model_key: String,
    pub segment: String,
}

/// A ref that left the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// Unix seconds.
    pub at: u64,
}

/// How many snapshots each ref keeps and when refs expire (RFC 0008 §5.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub default_snapshots: usize,
    pub branch_snapshots: usize,
    pub tag_snapshots: usize,
    /// A closed pull request expires this long after `closed_at`.
    pub pr_after_close: Duration,
    /// An unread pull request (or on-demand ref) expires after this long
    /// idle, judged from `reads/` evidence by the updater
    /// ([`crate::updater::Updater::expire_idle`]); zero disables it.
    pub pr_idle: Duration,
    /// Unreferenced objects younger than this are never deleted.
    pub grace: Duration,
}

impl Default for Retention {
    fn default() -> Self {
        const DAY: u64 = 24 * 3600;
        Retention {
            default_snapshots: 2,
            branch_snapshots: 1,
            tag_snapshots: 1,
            pr_after_close: Duration::from_secs(7 * DAY),
            pr_idle: Duration::from_secs(14 * DAY),
            grace: Duration::from_secs(DAY),
        }
    }
}

impl Retention {
    /// Snapshots kept for a ref of `kind` (at least 1); pull requests and
    /// on-demand refs keep 1.
    pub fn keep(&self, kind: RefKind) -> usize {
        let n = match kind {
            RefKind::Default => self.default_snapshots,
            RefKind::Branch => self.branch_snapshots,
            RefKind::Tag => self.tag_snapshots,
            RefKind::Pr | RefKind::Commit => 1,
        };
        n.max(1)
    }
}

impl RepoCatalog {
    pub fn new(repo: &str) -> RepoCatalog {
        RepoCatalog {
            format: CATALOG_FORMAT,
            repo: repo.to_string(),
            url: None,
            default_ref: None,
            refs: BTreeMap::new(),
            removed: Vec::new(),
        }
    }

    /// Whether ref `ref_name` already has a clean snapshot of `commit`.
    pub fn has_snapshot(&self, ref_name: &str, commit: &str) -> bool {
        self.refs.get(ref_name).is_some_and(|r| r.snapshots.iter().any(|s| !s.dirty && s.commit == commit))
    }

    /// Puts `snap` at the head of ref `ref_name` (created as `kind`; an
    /// existing ref takes `kind`), trims the ref to `retention` and returns
    /// the snapshots trimmed. A [`RefKind::Default`] ref becomes
    /// `default_ref`; a re-published ref leaves `removed`.
    pub fn insert(
        &mut self,
        ref_name: &str,
        kind: RefKind,
        snap: SnapshotEntry,
        retention: &Retention,
    ) -> Vec<SnapshotEntry> {
        if kind == RefKind::Default {
            self.default_ref = Some(ref_name.to_string());
        }
        self.removed.retain(|r| r.ref_name != ref_name);
        let entry = self.refs.entry(ref_name.to_string()).or_insert_with(|| RefEntry {
            kind,
            snapshots: Vec::new(),
            closed_at: None,
        });
        entry.kind = kind;
        entry.snapshots.insert(0, snap);
        let keep = retention.keep(kind);
        if entry.snapshots.len() > keep { entry.snapshots.split_off(keep) } else { Vec::new() }
    }

    /// Applies `retention` at `now` (unix seconds): trims every ref and
    /// moves expired refs to `removed`. Returns the expired ref names and
    /// whether anything changed.
    ///
    /// Expiry: a pull request whose `closed_at` is at least
    /// [`Retention::pr_after_close`] old. Idle expiry
    /// ([`Retention::pr_idle`]) needs read evidence: the updater applies it
    /// ([`crate::updater::Updater::expire_idle`]).
    pub fn apply_retention(&mut self, retention: &Retention, now: u64) -> (Vec<String>, bool) {
        let mut changed = false;
        for entry in self.refs.values_mut() {
            let keep = retention.keep(entry.kind);
            if entry.snapshots.len() > keep {
                entry.snapshots.truncate(keep);
                changed = true;
            }
        }
        let after = retention.pr_after_close.as_secs();
        let expired: Vec<String> = self
            .refs
            .iter()
            .filter(|(_, e)| e.kind == RefKind::Pr && e.closed_at.is_some_and(|t| now.saturating_sub(t) >= after))
            .map(|(name, _)| name.clone())
            .collect();
        for name in &expired {
            self.refs.remove(name);
            self.removed.push(Removed { ref_name: name.clone(), at: now });
        }
        if self.removed.len() > REMOVED_KEPT {
            let excess = self.removed.len() - REMOVED_KEPT;
            self.removed.drain(..excess);
        }
        let changed = changed || !expired.is_empty();
        (expired, changed)
    }

    /// The commit at the head of ref `ref_name` (what queries read).
    pub fn head_commit(&self, ref_name: &str) -> Option<&str> {
        self.refs.get(ref_name).and_then(|r| r.snapshots.first()).map(|s| s.commit.as_str())
    }

    /// Moves ref `ref_name` to `removed` at `now` (phase 2: expiry by the
    /// refs lifecycle, `DELETE /admin/refs`); whether it was there. Its
    /// objects go at the next sweep past the grace.
    pub fn remove_ref(&mut self, ref_name: &str, now: u64) -> bool {
        if self.refs.remove(ref_name).is_none() {
            return false;
        }
        self.removed.retain(|r| r.ref_name != ref_name);
        self.removed.push(Removed { ref_name: ref_name.to_string(), at: now });
        if self.removed.len() > REMOVED_KEPT {
            let excess = self.removed.len() - REMOVED_KEPT;
            self.removed.drain(..excess);
        }
        true
    }

    /// Records that pull request `ref_name` closed at `now` (kept when
    /// already set); whether anything changed.
    pub fn close_pr(&mut self, ref_name: &str, now: u64) -> bool {
        match self.refs.get_mut(ref_name) {
            Some(e) if e.kind == RefKind::Pr && e.closed_at.is_none() => {
                e.closed_at = Some(now);
                true
            }
            _ => false,
        }
    }

    /// Every snapshot of every ref.
    pub fn snapshots(&self) -> impl Iterator<Item = &SnapshotEntry> + '_ {
        self.refs.values().flat_map(|r| r.snapshots.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(commit: &str) -> SnapshotEntry {
        SnapshotEntry {
            commit: commit.into(),
            segment: format!("seg-{commit}"),
            indexed_at: 1,
            rueca_version: "t".into(),
            bytes: 1,
            packs: vec!["p".into()],
            vectors: None,
            dirty: false,
        }
    }

    #[test]
    fn insert_trims_per_kind_and_detects_duplicates() {
        let r = Retention::default();
        let mut c = RepoCatalog::new("x");
        assert!(c.insert("main", RefKind::Default, snap("a"), &r).is_empty());
        assert!(c.insert("main", RefKind::Default, snap("b"), &r).is_empty());
        let trimmed = c.insert("main", RefKind::Default, snap("c"), &r);
        assert_eq!(trimmed.iter().map(|s| s.commit.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(c.default_ref.as_deref(), Some("main"));
        assert!(c.has_snapshot("main", "b") && !c.has_snapshot("main", "a"));
        c.insert("pr/1", RefKind::Pr, snap("d"), &r);
        assert_eq!(c.insert("pr/1", RefKind::Pr, snap("e"), &r).len(), 1);
    }

    #[test]
    fn closed_pull_requests_expire_after_the_delay() {
        let r = Retention::default();
        let mut c = RepoCatalog::new("x");
        c.insert("pr/1", RefKind::Pr, snap("a"), &r);
        c.insert("pr/2", RefKind::Pr, snap("b"), &r);
        c.refs.get_mut("pr/1").unwrap().closed_at = Some(1_000);
        let week = r.pr_after_close.as_secs();
        assert_eq!(c.apply_retention(&r, 1_000 + week - 1), (vec![], false));
        assert_eq!(c.apply_retention(&r, 1_000 + week), (vec!["pr/1".to_string()], true));
        assert_eq!(c.removed, vec![Removed { ref_name: "pr/1".into(), at: 1_000 + week }]);
        assert!(c.refs.contains_key("pr/2"));
        // Re-publishing brings the ref back.
        c.insert("pr/1", RefKind::Pr, snap("c"), &r);
        assert!(c.removed.is_empty());
    }

    #[test]
    fn catalog_json_shape() {
        let mut c = RepoCatalog::new("github.com/acme/billing");
        c.insert("main", RefKind::Default, snap("a"), &Retention::default());
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["refs"]["main"]["kind"], "default");
        assert_eq!(v["default_ref"], "main");
        assert!(v["refs"]["main"]["snapshots"][0].get("dirty").is_none());
        let back: RepoCatalog = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
    }
}
