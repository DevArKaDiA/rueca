//! `rueca index --org` end to end on real working trees: every member is
//! indexed with the repository pipeline, then the organization layer is
//! built; re-indexing one member replaces only its part.

use std::fs;
use std::path::Path;

use rueca_index::org::{
    OrgConfig, OrgIndexOptions, index_org, index_org_ref, update_orgs_containing, update_orgs_of_repo,
};
use rueca_index::refs::{self, RefIndexOptions, RefKind};
use rueca_index::{IndexOptions, index_worktree};
use rueca_segment::Store;
use rueca_segment::org::{OrgGeneration, current_generation};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn index_org_then_one_member() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    write(&base.join("billing"), "billing/invoice.py", "def issue_invoice(order):\n    return order\n");
    write(&base.join("payments"), "payments/charge.py", "def charge_card(card):\n    return card\n");
    let file = base.join("rueca-org.toml");
    fs::write(
        &file,
        "name = \"acme\"\n[[repo]]\npath = \"billing\"\n[[repo]]\npath = \"payments\"\nid = \"local/pay\"\n",
    )
    .unwrap();
    let config = OrgConfig::load(&file).unwrap();
    let opts = OrgIndexOptions {
        config: config.clone(),
        store: store.clone(),
        budget: Default::default(),
        threads: Some(2),
        only: None,
    };
    let r = index_org(&opts).unwrap();
    assert_eq!(r.indexed.len(), 2);
    assert_eq!(
        (r.update.generation, r.update.added.clone()),
        (1, vec!["local/billing".to_string(), "local/pay".to_string()])
    );
    let g = current_generation(&store, "acme").unwrap().unwrap();
    assert_eq!(g.repos.iter().map(|r| r.repo.as_str()).collect::<Vec<_>>(), ["local/billing", "local/pay"]);

    // One member changes: only it is re-indexed and replaced.
    write(&base.join("payments"), "payments/refund.py", "def refund(card):\n    return card\n");
    let only = OrgIndexOptions { only: Some(base.join("payments/payments")), ..opts.clone() };
    let r = index_org(&only).unwrap();
    assert_eq!(r.indexed.len(), 1);
    assert_eq!(
        (r.update.generation, r.update.replaced.clone(), r.update.unchanged),
        (2, vec!["local/pay".to_string()], 1)
    );
    let outside = OrgIndexOptions { only: Some(store.clone()), ..opts.clone() };
    assert!(index_org(&outside).unwrap_err().to_string().contains("is not a repository of organization"));

    // A plain `rueca index <member>` updates the organizations holding it.
    write(&base.join("billing"), "billing/tax.py", "def tax(order):\n    return 0\n");
    index_worktree(&IndexOptions {
        root: base.join("billing"),
        store: store.clone(),
        repo: None,
        budget: Default::default(),
        threads: Some(2),
        ..IndexOptions::default()
    })
    .unwrap();
    let ups = update_orgs_containing(&store, &base.join("billing")).unwrap();
    assert_eq!(ups.len(), 1);
    assert_eq!((ups[0].generation, ups[0].replaced.clone()), (3, vec!["local/billing".to_string()]));
    assert!(update_orgs_containing(&store, &base).unwrap().is_empty());
}

/// A `url` member is cloned under the store (needs the `git` executable).
#[test]
fn url_members_are_cloned() {
    let has_git = std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success());
    if !has_git {
        eprintln!("git not available; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let remote = base.join("remote");
    write(&remote, "lib.py", "def shared_helper():\n    return 1\n");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&remote)
            .args(["-c", "user.name=t", "-c", "user.email=t@invalid", "-c", "commit.gpgsign=false"])
            .args(["-c", "tag.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["checkout", "-q", "-b", "next"]);
    write(&remote, "next.py", "def next_helper():\n    return 3\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "next"]);
    // A tag on that commit: the shallow clone of `main` does not have it.
    git(&["tag", "-a", "v-next", "-m", "release"]);
    git(&["checkout", "-q", "-b", "side", "main"]);
    write(&remote, "side.py", "def side_helper():\n    return 5\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "side"]);
    git(&["checkout", "-q", "main"]);
    let side = String::from_utf8(
        std::process::Command::new("git").arg("-C").arg(&remote).args(["rev-parse", "side"]).output().unwrap().stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let url = format!("file://{}", remote.display());
    let store = base.join("store");
    let config = OrgConfig::parse(
        &format!("name = \"acme\"\n[[repo]]\nurl = \"{url}\"\nid = \"local/remote\"\nrefs = [\"next\", \"v-next\"]\n"),
        &base,
    )
    .unwrap();
    let opts = OrgIndexOptions {
        config: config.clone(),
        store: store.clone(),
        budget: Default::default(),
        threads: Some(1),
        only: None,
    };
    let r = index_org(&opts).unwrap();
    assert_eq!(r.update.added, ["local/remote"]);
    // The branch and the tag of the remote (absent from the shallow clone)
    // are fetched.
    assert!(r.ref_errors.is_empty(), "{:?}", r.ref_errors);
    assert_eq!(r.update.refs_added, ["local/remote@next", "local/remote@v-next"]);
    assert_eq!(
        (r.refs[1].kind, r.refs[1].fetched, r.refs[1].commit.as_str()),
        (RefKind::Tag, true, r.refs[0].commit.as_str())
    );
    let next_commit = r.refs[0].commit.clone();
    let clone = config.root_of(&config.repos[0], &store);
    assert!(clone.join("lib.py").exists() && clone.starts_with(store.join("orgs/acme/clones")));
    // A second run refreshes the clone.
    write(&remote, "more.py", "def more():\n    return 2\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "more"]);
    let r = index_org(&opts).unwrap();
    assert!(clone.join("more.py").exists());
    assert_eq!(r.update.replaced, ["local/remote"]);
    assert!(r.refs[0].unchanged && r.refs[1].unchanged && r.update.refs_unchanged == 2, "{:?}", r.update);
    // A new commit on the remote's branch is followed.
    git(&["checkout", "-q", "next"]);
    write(&remote, "later.py", "def later():\n    return 4\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "later"]);
    git(&["checkout", "-q", "main"]);
    let r = index_org(&opts).unwrap();
    assert_ne!(r.refs[0].commit, next_commit);
    assert_eq!(r.update.refs_replaced, ["local/remote@next"]);
    let g = current_generation(&store, "acme").unwrap().unwrap();
    assert_eq!(g.repos[0].url.as_deref(), Some(url.as_str()));
    assert_eq!(g.refs_of(0).iter().map(|x| x.ref_name.as_str()).collect::<Vec<_>>(), ["main", "next", "v-next"]);
    // One ref of a `url` member by its id.
    let by_id = OrgIndexOptions { only: Some("local/remote".into()), ..opts };
    let r = index_org_ref(&by_id, "next", true).unwrap();
    assert_eq!(r.update.refs_removed, ["local/remote@next"]);
    // A commit the clone has never seen, by its full id.
    let r = index_org_ref(&by_id, &side, false).unwrap();
    let c = r.indexed.unwrap();
    assert_eq!(
        (c.kind, c.fetched, c.ref_name.as_str(), c.commit.as_str()),
        (RefKind::Commit, true, &side[..12], side.as_str())
    );
    // A name that is not a ref is refused before git runs.
    let e = index_org_ref(&by_id, "-x", false).unwrap_err().to_string();
    assert!(e.contains("not a valid ref name"), "{e}");
}

/// Runs git in `dir` with a fixed identity; `false` when it fails or git is
/// missing.
fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn commit_all(dir: &Path, msg: &str) {
    assert!(git(dir, &["add", "-A"]));
    assert!(git(dir, &["commit", "-q", "-m", msg]));
}

/// A git repository at `root` on `main` with branch `branch` adding
/// `extra`; `main` checked out. `None` without git.
fn repo_with_branch(root: &Path, files: &[(&str, &str)], branch: &str, extra: (&str, &str)) -> Option<()> {
    fs::create_dir_all(root).unwrap();
    if !git(root, &["init", "-q"]) {
        eprintln!("git not available; skipping");
        return None;
    }
    assert!(git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]));
    for (p, t) in files {
        write(root, p, t);
    }
    commit_all(root, "init");
    assert!(git(root, &["checkout", "-q", "-b", branch]));
    write(root, extra.0, extra.1);
    commit_all(root, "branch");
    assert!(git(root, &["checkout", "-q", "main"]));
    Some(())
}

/// Commits `file` on `branch` of `root` and checks `main` out again.
fn commit_on(root: &Path, branch: &str, file: (&str, &str)) {
    assert!(git(root, &["checkout", "-q", branch]));
    write(root, file.0, file.1);
    commit_all(root, "more");
    assert!(git(root, &["checkout", "-q", "main"]));
}

/// `(repo, ref, since_generation, segment)` of every entry of `g`.
fn entries(g: &OrgGeneration) -> Vec<(String, String, u64, String)> {
    g.slots()
        .into_iter()
        .map(|s| {
            let e = g.entry(s).unwrap();
            (g.repos[s.repo].repo.clone(), g.ref_name(s).unwrap(), e.since_generation, e.segment_id.clone())
        })
        .collect()
}

#[test]
fn refs_in_an_organization() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let store = base.join("store");
    let (billing, payments) = (base.join("billing"), base.join("payments"));
    let tax = ("billing/tax.py", "def compute_tax(order):\n    return 0\n");
    let invoice = [("billing/invoice.py", "def issue_invoice(order):\n    return order\n")];
    if repo_with_branch(&billing, &invoice, "feature/tax", tax).is_none() {
        return;
    }
    let charge = [("payments/charge.py", "def charge_card(card):\n    return card\n")];
    repo_with_branch(&payments, &charge, "feature/refund", ("payments/refund.py", "def refund(c):\n    return c\n"))
        .unwrap();
    let file = base.join("rueca-org.toml");
    fs::write(
        &file,
        "name = \"acme\"\n[[repo]]\npath = \"billing\"\nrefs = [\"feature/tax\"]\n\
         [[repo]]\npath = \"payments\"\nrefs = [\"feature/refund\"]\n",
    )
    .unwrap();
    let opts = OrgIndexOptions {
        config: OrgConfig::load(&file).unwrap(),
        store: store.clone(),
        budget: Default::default(),
        threads: Some(2),
        only: None,
    };

    // Every member at its default and at its branch.
    let r = index_org(&opts).unwrap();
    assert!(r.ref_errors.is_empty(), "{:?}", r.ref_errors);
    assert_eq!(r.refs.len(), 2);
    assert_eq!((r.update.generation, r.update.repos, r.update.refs), (1, 2, 2));
    assert_eq!(r.update.refs_added, ["local/billing@feature/tax", "local/payments@feature/refund"]);
    let g1 = current_generation(&store, "acme").unwrap().unwrap();
    let e1 = entries(&g1);
    let names: Vec<(&str, &str)> = e1.iter().map(|e| (e.0.as_str(), e.1.as_str())).collect();
    assert_eq!(
        names,
        [
            ("local/billing", "main"),
            ("local/billing", "feature/tax"),
            ("local/payments", "main"),
            ("local/payments", "feature/refund")
        ]
    );
    eprintln!(
        "first generation: org layer {} ms (refs {} ms, repositories {} ms); costs {:?}",
        r.org_ms, r.refs_ms, r.repos_ms, r.update.costs
    );

    // A new commit on one branch: refreshing that ref replaces its entry
    // only; every other entry keeps its segment and generation.
    commit_on(&payments, "feature/refund", ("payments/dispute.py", "def dispute(c):\n    return c\n"));
    let one = OrgIndexOptions { only: Some(payments.clone()), ..opts.clone() };
    let r = index_org_ref(&one, "feature/refund", false).unwrap();
    assert!(!r.indexed.as_ref().unwrap().unchanged);
    assert_eq!(
        (r.update.generation, r.update.refs_replaced.clone()),
        (2, vec!["local/payments@feature/refund".into()])
    );
    assert!(r.update.replaced.is_empty() && r.update.linked.is_empty(), "{:?}", r.update);
    assert_eq!((r.update.unchanged, r.update.refs_unchanged), (2, 1));
    eprintln!("refresh one ref: org layer {} ms (ref {} ms); costs {:?}", r.org_ms, r.ref_ms, r.update.costs);
    let g2 = current_generation(&store, "acme").unwrap().unwrap();
    let e2 = entries(&g2);
    for (a, b) in e1.iter().zip(&e2) {
        if b.1 == "feature/refund" {
            assert_eq!(b.2, 2);
            assert_ne!(a.3, b.3);
        } else {
            assert_eq!(a, b, "untouched entries are reused");
        }
    }
    // Unchanged ref: nothing published.
    let r = index_org_ref(&one, "feature/refund", false).unwrap();
    assert!(r.indexed.unwrap().unchanged && !r.update.published);

    // A ref added from the command line is listed, and refreshed by the
    // next `index --org` of its member (with the file's refs).
    let pin = refs::index_ref(&RefIndexOptions {
        root: billing.clone(),
        name: "HEAD".into(),
        store: store.clone(),
        threads: Some(1),
        ..RefIndexOptions::default()
    })
    .unwrap();
    let ups = update_orgs_of_repo(&store, "local/billing").unwrap();
    assert_eq!(ups[0].refs_added, [format!("local/billing@{}", pin.ref_name)]);
    let r = index_org(&OrgIndexOptions { only: Some(billing.clone()), ..opts.clone() }).unwrap();
    let synced: Vec<&str> = r.refs.iter().map(|x| x.ref_name.as_str()).collect();
    assert_eq!(synced, ["feature/tax", pin.ref_name.as_str()]);
    assert!(r.refs.iter().all(|x| x.unchanged));
    assert_eq!((r.update.replaced.clone(), r.update.refs_replaced.len()), (vec!["local/billing".to_string()], 0));

    // `--remove`: the entry goes next generation; its segment, profile and
    // tantivy files once no kept generation reads them.
    let g = current_generation(&store, "acme").unwrap().unwrap();
    let pay = g.repo_index("local/payments").unwrap();
    let gone = g.entry(g.find_ref(pay, "feature/refund").unwrap()).unwrap().clone();
    let r = index_org_ref(&one, "feature/refund", true).unwrap();
    assert_eq!(r.update.refs_removed, ["local/payments@feature/refund"]);
    assert!(r.removed.unwrap().export_removed);
    let g = current_generation(&store, "acme").unwrap().unwrap();
    assert!(g.find_ref(pay, "feature/refund").is_none());
    assert!(Store::open(&store).unwrap().segment_path(&gone.segment_id).exists(), "the previous generation reads it");
    // The file still lists it: `index --org` re-adds it (and the earlier
    // generations holding the removed entry are collected).
    commit_on(&billing, "feature/tax", ("billing/vat.py", "def vat():\n    return 1\n"));
    let r = index_org(&OrgIndexOptions { only: Some(payments.clone()), ..opts.clone() }).unwrap();
    assert_eq!(r.update.refs_added, ["local/payments@feature/refund"]);
    index_org(&OrgIndexOptions { only: Some(billing.clone()), ..opts.clone() }).unwrap();
    let org = store.join("orgs/acme");
    assert!(!Store::open(&store).unwrap().segment_path(&gone.segment_id).exists());
    assert!(!org.join("profiles").join(gone.profile.unwrap()).exists());
    let uuid = gone.tantivy_segment.unwrap();
    assert!(
        fs::read_dir(org.join("index")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(&uuid))
    );

    // Errors: a member that is not one, a ref that does not exist.
    let bad = OrgIndexOptions { only: Some("local/nope".into()), ..opts.clone() };
    assert!(index_org_ref(&bad, "main", false).unwrap_err().to_string().contains("is not a repository"));
    let r = index_org_ref(&one, "no-such-branch", false);
    assert!(r.unwrap_err().to_string().contains("not found"));
    // A configured ref that cannot be indexed is reported, not fatal.
    fs::write(
        &file,
        "name = \"acme\"\n[[repo]]\npath = \"billing\"\nrefs = [\"feature/tax\", \"missing/branch\"]\n\
         [[repo]]\npath = \"payments\"\n",
    )
    .unwrap();
    let cfg = OrgIndexOptions { config: OrgConfig::load(&file).unwrap(), only: Some(billing.clone()), ..opts };
    let r = index_org(&cfg).unwrap();
    assert_eq!(r.ref_errors.len(), 1, "{:?}", r.ref_errors);
    assert!(r.update.missing.contains(&"local/billing@missing/branch".to_string()), "{:?}", r.update.missing);
}

/// A member path through a symbolic link (macOS `/tmp` → `/private/tmp`)
/// derives the git id of its target for the default and its refs alike,
/// so the organization lists the ref.
#[cfg(unix)]
#[test]
fn symlinked_members_keep_their_git_id() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let real = base.join("real/click");
    if repo_with_branch(
        &real,
        &[("click/core.py", "def main():\n    return 1\n")],
        "feature",
        ("x.py", "def x():\n    return 2\n"),
    )
    .is_none()
    {
        return;
    }
    assert!(git(&real, &["remote", "add", "origin", "https://github.com/pallets/click.git"]));
    std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
    let linked = base.join("link/click");
    assert_eq!(rueca_index::repo_id(&linked, None), "github.com/pallets/click");
    assert_eq!(refs::root_prefix(&linked), "");
    let file = base.join("rueca-org.toml");
    fs::write(&file, format!("name = \"acme\"\n[[repo]]\npath = \"{}\"\n", linked.display())).unwrap();
    let opts = OrgIndexOptions {
        config: OrgConfig::load(&file).unwrap(),
        store: base.join("store"),
        budget: Default::default(),
        threads: Some(1),
        only: None,
    };
    let r = index_org(&opts).unwrap();
    assert_eq!(r.update.added, ["github.com/pallets/click"]);
    let one = OrgIndexOptions { only: Some(linked.clone()), ..opts.clone() };
    let r = index_org_ref(&one, "feature", false).unwrap();
    assert_eq!(r.repo, "github.com/pallets/click");
    assert_eq!(r.update.refs_added, ["github.com/pallets/click@feature"], "{:?}", r.update);
    assert!(r.update.unowned_refs.is_empty(), "{:?}", r.update.unowned_refs);
    // Removing it through the link finds the same export.
    let r = index_org_ref(&one, "feature", true).unwrap();
    assert_eq!(r.update.refs_removed, ["github.com/pallets/click@feature"]);
}

/// A ref the organization cannot list (its member's default was never
/// indexed) is reported, not dropped silently.
#[test]
fn refs_of_no_member_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let root = base.join("click");
    if repo_with_branch(
        &root,
        &[("core.py", "def main():\n    return 1\n")],
        "feature",
        ("x.py", "def x():\n    return 2\n"),
    )
    .is_none()
    {
        return;
    }
    let file = base.join("rueca-org.toml");
    fs::write(&file, "name = \"acme\"\n[[repo]]\npath = \"click\"\n").unwrap();
    let opts = OrgIndexOptions {
        config: OrgConfig::load(&file).unwrap(),
        store: base.join("store"),
        budget: Default::default(),
        threads: Some(1),
        only: Some(root.clone()),
    };
    let r = index_org_ref(&opts, "feature", false).unwrap();
    assert!(r.update.refs_added.is_empty());
    assert_eq!(
        r.update.unowned_refs,
        ["ref feature of local/click belongs to no indexed member of organization `acme`"],
        "{:?}",
        r.update
    );
    let json = serde_json::to_value(&r.update).unwrap();
    assert_eq!(json["unowned_refs"][0], r.update.unowned_refs[0]);
    // Once the member is indexed, the ref is listed and nothing is reported.
    let r = index_org(&opts).unwrap();
    assert_eq!(r.update.refs_added, ["local/click@feature"]);
    assert!(r.update.unowned_refs.is_empty(), "{:?}", r.update.unowned_refs);
}
