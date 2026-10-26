//! `rueca index --org` end to end on real working trees: every member is
//! indexed with the repository pipeline, then the organization layer is
//! built; re-indexing one member replaces only its part.

use std::fs;
use std::path::Path;

use rueca_index::org::{OrgConfig, OrgIndexOptions, index_org, update_orgs_containing};
use rueca_index::{IndexOptions, index_worktree};
use rueca_segment::org::current_generation;

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
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    let url = format!("file://{}", remote.display());
    let store = base.join("store");
    let config =
        OrgConfig::parse(&format!("name = \"acme\"\n[[repo]]\nurl = \"{url}\"\nid = \"local/remote\"\n"), &base)
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
    let clone = config.root_of(&config.repos[0], &store);
    assert!(clone.join("lib.py").exists() && clone.starts_with(store.join("orgs/acme/clones")));
    // A second run refreshes the clone.
    write(&remote, "more.py", "def more():\n    return 2\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "more"]);
    let r = index_org(&opts).unwrap();
    assert!(clone.join("more.py").exists());
    assert_eq!(r.update.replaced, ["local/remote"]);
    let g = current_generation(&store, "acme").unwrap().unwrap();
    assert_eq!(g.repos[0].url.as_deref(), Some(url.as_str()));
}
