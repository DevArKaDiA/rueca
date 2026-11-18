//! The hand-written payloads of `tests/fixtures/webhooks/` (documented
//! shapes, RFC 0008 §7.3) parsed into hints, and each event's change to the
//! desired state against a host listing (`plan`).

use rueca_service::RefKind;
use rueca_service::reconcile::RepoSpec;
use rueca_service::remote::RemoteRefs;
use rueca_service::webhooks::{Change, Hint, Parsed, RefHint, WebhookHost, match_repo, parse, plan};

const URL: &str = "https://github.com/acme/billing.git";

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/webhooks/{name}.json", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().replace("REPO_URL", URL)
}

fn hint(host: WebhookHost, event: &str, name: &str) -> Hint {
    match parse(host, event, fixture(name).as_bytes()).unwrap() {
        Parsed::Hint(h) => h,
        Parsed::Ignored(why) => panic!("{name}: ignored ({why})"),
    }
}

fn spec() -> RepoSpec {
    RepoSpec::new(URL, None, &["release/*".to_string()], &["v*".to_string()], true).unwrap()
}

/// What the host lists: `main`, `feature`, the tag `v2`, and the pull
/// request head refs of GitHub and GitLab.
fn listing() -> RemoteRefs {
    let refs = [
        ("refs/heads/main", "m"),
        ("refs/heads/feature", "f"),
        ("refs/tags/v2", "t"),
        ("refs/pull/7/head", "p"),
        ("refs/merge-requests/7/head", "p"),
    ];
    RemoteRefs { head: Some("main".into()), refs: refs.iter().map(|(r, c)| (r.to_string(), c.to_string())).collect() }
}

fn changes(h: &Hint) -> Vec<Change> {
    plan(&spec(), None, &listing(), h).changes
}

fn set(name: &str, kind: RefKind, commit: &str) -> Change {
    Change::Set { name: name.into(), kind, commit: commit.into() }
}

#[test]
fn github_events() {
    let push = hint(WebhookHost::GitHub, "push", "github-push");
    assert_eq!(push.refs, [RefHint::Ref { full: "refs/heads/main".into(), deleted: false }]);
    assert_eq!(match_repo(&[spec()], &push).map(|s| s.repo.as_str()), Some("github.com/acme/billing"));
    assert_eq!(changes(&push), [set("main", RefKind::Default, "m")], "the listed commit, not the payload's");
    let tag = hint(WebhookHost::GitHub, "create", "github-create-tag");
    assert_eq!(changes(&tag), [set("v2", RefKind::Tag, "t")]);
    let deleted = hint(WebhookHost::GitHub, "delete", "github-delete-branch");
    assert_eq!(changes(&deleted), [Change::Remove { name: "release/1".into() }]);
    let opened = hint(WebhookHost::GitHub, "pull_request", "github-pull_request-opened");
    assert_eq!(changes(&opened), [set("pr/7", RefKind::Pr, "p")]);
    let closed = hint(WebhookHost::GitHub, "pull_request", "github-pull_request-closed");
    assert_eq!(changes(&closed), [Change::Close { name: "pr/7".into() }]);
    let ping = parse(WebhookHost::GitHub, "ping", fixture("github-ping").as_bytes()).unwrap();
    assert!(matches!(ping, Parsed::Ignored(_)), "{ping:?}");
}

#[test]
fn gitlab_events() {
    let push = hint(WebhookHost::GitLab, "Push Hook", "gitlab-push");
    assert_eq!(changes(&push), [set("main", RefKind::Default, "m")]);
    let tag = hint(WebhookHost::GitLab, "Tag Push Hook", "gitlab-tag_push");
    assert_eq!(changes(&tag), [set("v2", RefKind::Tag, "t")]);
    let open = hint(WebhookHost::GitLab, "Merge Request Hook", "gitlab-merge_request-open");
    assert_eq!(open.refs, [RefHint::PrHead { number: 7, source_branch: Some("feature".into()) }]);
    assert_eq!(changes(&open), [set("pr/7", RefKind::Pr, "p")]);
    let merged = hint(WebhookHost::GitLab, "Merge Request Hook", "gitlab-merge_request-merge");
    assert_eq!(changes(&merged), [Change::Close { name: "pr/7".into() }]);
}

#[test]
fn bitbucket_events() {
    let push = hint(WebhookHost::Bitbucket, "repo:push", "bitbucket-repo_push");
    assert_eq!(changes(&push), [set("main", RefKind::Default, "m"), Change::Remove { name: "release/1".into() }]);
    // Bitbucket Cloud lists no pull request refs: the source branch's tip.
    let mut created = hint(WebhookHost::Bitbucket, "pullrequest:created", "bitbucket-pullrequest_created");
    let without_pr_refs = RemoteRefs {
        head: Some("main".into()),
        refs: [("refs/heads/feature".to_string(), "f".to_string())].into_iter().collect(),
    };
    assert_eq!(plan(&spec(), None, &without_pr_refs, &created).changes, [set("pr/7", RefKind::Pr, "f")]);
    // A fork's pull request cannot be confirmed.
    created.refs = vec![RefHint::PrHead { number: 7, source_branch: None }];
    assert!(plan(&spec(), None, &without_pr_refs, &created).changes.is_empty());
    let fulfilled = hint(WebhookHost::Bitbucket, "pullrequest:fulfilled", "bitbucket-pullrequest_fulfilled");
    assert_eq!(changes(&fulfilled), [Change::Close { name: "pr/7".into() }]);
}

#[test]
fn malformed_and_foreign_payloads() {
    assert!(parse(WebhookHost::GitHub, "push", b"not json").is_err());
    assert!(parse(WebhookHost::GitHub, "push", b"{\"repository\": {}}").is_err(), "no ref");
    let mut other = hint(WebhookHost::GitHub, "push", "github-push");
    other.urls = vec!["https://github.com/acme/elsewhere.git".into()];
    assert!(match_repo(&[spec()], &other).is_none());
}
