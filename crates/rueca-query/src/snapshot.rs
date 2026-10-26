//! Snapshot descriptors (RFC 0005 §3–§5): what the envelope says about each
//! snapshot a response read, the pinned short commit used in ids, ref
//! matching and host URL templates.
//!
//! Local mode has one snapshot per indexed root: the working tree, named by
//! its checked-out branch (`main`), `HEAD` when detached, or `worktree`
//! outside git. Everything here takes a list of snapshots per repository, so
//! a catalog with several snapshots per (repository, ref) slots in unchanged.

use rueca_model::SnapshotInfo;
use serde_json::{Value, json};

/// Hex digits of a commit kept in ids (git's convention for unambiguous short
/// ids in large repositories).
pub const SHORT_COMMIT: usize = 12;

/// Shortest commit prefix accepted in ids and `ref`.
pub const MIN_COMMIT_PREFIX: usize = 7;

/// Ref name of a local snapshot outside git.
pub const WORKTREE_REF: &str = "worktree";

/// One snapshot as the envelope describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDesc {
    /// Repository id (`github.com/o/r`, `local/<dir>`).
    pub repo: String,
    /// Ref the snapshot was taken from: the branch, `HEAD` (detached) or `worktree` (no git).
    pub ref_name: String,
    /// Full commit (`"worktree"` outside git).
    pub commit: String,
    /// The snapshot includes uncommitted changes on top of `commit` (RFC 0001 §11.2 overlay).
    pub overlay: bool,
    /// Indexed root (local mode).
    pub root: Option<String>,
}

impl SnapshotDesc {
    pub fn from_info(info: &SnapshotInfo) -> SnapshotDesc {
        let real_commit = is_hex(&info.commit) && info.commit.len() >= MIN_COMMIT_PREFIX;
        let ref_name = match &info.branch {
            Some(b) if !b.is_empty() => b.clone(),
            _ if real_commit => "HEAD".to_string(),
            _ => WORKTREE_REF.to_string(),
        };
        SnapshotDesc {
            repo: info.repo.clone(),
            ref_name,
            commit: info.commit.clone(),
            // Outside git (or before the first commit) there is no base to
            // overlay: the whole tree is the snapshot.
            overlay: info.dirty && real_commit,
            root: info.root.clone(),
        }
    }

    /// The commit as ids carry it: 12 hex digits, plus `+` for an overlay.
    pub fn id_commit(&self) -> String {
        let mut c = short_commit(&self.commit).to_string();
        if self.overlay {
            c.push('+');
        }
        c
    }

    /// `repo@commit12[+]`, the prefix of every id of this snapshot.
    pub fn id_prefix(&self) -> String {
        format!("{}@{}", self.repo, self.id_commit())
    }

    /// Whether `r` names this snapshot: its ref name (`main`, `refs/heads/main`), `HEAD`,
    /// `worktree` (local mode: the working tree is the default ref), or its commit (the full
    /// id, a prefix of at least 7 hex digits, or the 12-digit form with the overlay `+`).
    pub fn matches_ref(&self, r: &str) -> bool {
        let r = r.trim();
        if r.is_empty() {
            return false;
        }
        if r == self.ref_name || r == "HEAD" || r == WORKTREE_REF {
            return true;
        }
        if let Some(b) = r.strip_prefix("refs/heads/")
            && b == self.ref_name
        {
            return true;
        }
        self.matches_commit(r)
    }

    /// Whether `c` (optionally ending in the overlay `+`) is this snapshot's commit: equal,
    /// or a hex prefix of at least [`MIN_COMMIT_PREFIX`] digits.
    pub fn matches_commit(&self, c: &str) -> bool {
        let c = c.strip_suffix('+').unwrap_or(c);
        if c.eq_ignore_ascii_case(&self.commit) {
            return true;
        }
        c.len() >= MIN_COMMIT_PREFIX
            && is_hex(c)
            && self.commit.len() >= c.len()
            && self.commit[..c.len()].eq_ignore_ascii_case(c)
    }

    /// How an error message lists this snapshot: `main (7696364aef89+, working tree of /src/x)`.
    pub fn describe(&self) -> String {
        let mut s = format!("`{}` ({}", self.ref_name, self.id_commit());
        if let Some(root) = &self.root {
            s.push_str(&format!(", working tree of {root}"));
        }
        s.push(')');
        s
    }

    /// The envelope entry (`s` is the snapshot's index in this response).
    pub fn to_json(&self, s: usize) -> Value {
        let mut v = json!({
            "s": s,
            "repo": self.repo,
            "ref": self.ref_name,
            "commit": self.commit,
            "overlay": self.overlay,
        });
        if let Some(url) = url_template(&self.repo, &self.commit) {
            v["url"] = Value::String(url);
        }
        v
    }

    /// One header line of the text rendering: `github.com/o/r@7696364aef89+ (main)`.
    pub fn header(&self) -> String {
        format!("{} ({})", self.id_prefix(), self.ref_name)
    }
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The first [`SHORT_COMMIT`] hex digits of a commit; other strings (`worktree`, short test
/// commits) unchanged.
pub fn short_commit(commit: &str) -> &str {
    if commit.len() > SHORT_COMMIT && is_hex(commit) { &commit[..SHORT_COMMIT] } else { commit }
}

/// Template of a link to a line range on the repository's host, for repository ids of known
/// hosts (`github.com`, `gitlab.com`, `bitbucket.org`, and hosts named `github.*` / `gitlab.*`).
/// Placeholders: `{path}`, `{start}`, `{end}`. `None` for unknown hosts, `local/…` ids and
/// non-commit snapshots. With an overlay the link shows the base commit, whose lines may differ
/// in modified files.
pub fn url_template(repo: &str, commit: &str) -> Option<String> {
    if !(is_hex(commit) && commit.len() >= MIN_COMMIT_PREFIX) {
        return None;
    }
    let (host, path) = repo.split_once('/')?;
    if path.is_empty() || !path.contains('/') {
        return None;
    }
    let first = host.split('.').next().unwrap_or_default();
    if host == "github.com" || first == "github" {
        Some(format!("https://{host}/{path}/blob/{commit}/{{path}}#L{{start}}-L{{end}}"))
    } else if host == "gitlab.com" || first == "gitlab" {
        Some(format!("https://{host}/{path}/-/blob/{commit}/{{path}}#L{{start}}-{{end}}"))
    } else if host == "bitbucket.org" {
        Some(format!("https://{host}/{path}/src/{commit}/{{path}}#lines-{{start}}:{{end}}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "7696364aef89a22e4a2fe84f6fd23e186bef9aa8";

    fn info(commit: &str, dirty: bool, branch: Option<&str>) -> SnapshotInfo {
        SnapshotInfo {
            repo: "github.com/acme/billing".into(),
            commit: commit.into(),
            dirty,
            branch: branch.map(str::to_string),
            root: Some("/src/billing".into()),
            indexed_at: 0,
            rueca_version: "t".into(),
        }
    }

    #[test]
    fn ids_and_refs() {
        let d = SnapshotDesc::from_info(&info(C, true, Some("main")));
        assert_eq!(d.id_commit(), "7696364aef89+");
        assert_eq!(d.id_prefix(), "github.com/acme/billing@7696364aef89+");
        assert_eq!(d.header(), "github.com/acme/billing@7696364aef89+ (main)");
        for ok in ["main", "refs/heads/main", "HEAD", "worktree", C, "7696364", "7696364AEF89", "7696364aef89+"] {
            assert!(d.matches_ref(ok), "{ok}");
        }
        for bad in ["dev", "769636", "7696365", "pr/1", "", "refs/heads/dev"] {
            assert!(!d.matches_ref(bad), "{bad}");
        }
        let clean = SnapshotDesc::from_info(&info(C, false, None));
        assert_eq!((clean.ref_name.as_str(), clean.id_commit().as_str()), ("HEAD", "7696364aef89"));
        // Outside git: no base commit, so no overlay marker.
        let wt = SnapshotDesc::from_info(&info("worktree", true, None));
        assert_eq!((wt.ref_name.as_str(), wt.id_commit().as_str(), wt.overlay), ("worktree", "worktree", false));
        assert!(wt.matches_ref("worktree") && !wt.matches_ref("main"));
        assert_eq!(short_commit("abc123"), "abc123");
    }

    #[test]
    fn url_templates() {
        assert_eq!(
            url_template("github.com/acme/billing", C).unwrap(),
            format!("https://github.com/acme/billing/blob/{C}/{{path}}#L{{start}}-L{{end}}")
        );
        assert_eq!(
            url_template("gitlab.com/g/sub/r", C).unwrap(),
            format!("https://gitlab.com/g/sub/r/-/blob/{C}/{{path}}#L{{start}}-{{end}}")
        );
        assert_eq!(
            url_template("bitbucket.org/o/r", C).unwrap(),
            format!("https://bitbucket.org/o/r/src/{C}/{{path}}#lines-{{start}}:{{end}}")
        );
        assert!(url_template("gitlab.example.com/o/r", C).unwrap().starts_with("https://gitlab.example.com/o/r/-/"));
        assert_eq!(url_template("local/billing", C), None);
        assert_eq!(url_template("git.example.com/o/r", C), None);
        assert_eq!(url_template("github.com/o/r", "worktree"), None);
        let v = SnapshotDesc::from_info(&info(C, false, Some("main"))).to_json(0);
        assert_eq!(v["s"], 0);
        assert_eq!(v["ref"], "main");
        assert_eq!(v["commit"], C);
        assert!(v["url"].as_str().unwrap().contains("/blob/7696364aef89a22e"));
    }
}
