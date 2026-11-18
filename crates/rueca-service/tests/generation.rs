//! Two updaters publishing a generation from the same base: exactly one
//! wins, the other gets a `Conflict` and publishes after rebuilding on the
//! winner's generation (RFC 0008 §5.4).

mod common;

use std::sync::Barrier;

use common::{commit, open, write};
use rueca_segment::org::{current_generation, update_org};
use rueca_service::{
    Conflict, PullWhich, RefKind, Retention, ServiceStore, publish_generation, publish_snapshot, pull_generation,
    pull_repo,
};

const URL: &str = "memory://generation-conflict";
const REPOS: [&str; 3] = ["github.com/acme/core", "github.com/acme/billing", "github.com/acme/payments"];

/// Pulls `repos` (default refs) into `mirror`, then rebuilds the
/// organization from everything the mirror holds.
fn rebuild(mirror: &ServiceStore, repos: &[&str]) -> u64 {
    for repo in repos {
        pull_repo(mirror, repo, PullWhich::Default).unwrap();
    }
    update_org(&mirror.local, "acme", &mirror.mirror_members().unwrap()).unwrap().generation
}

#[test]
fn one_of_two_updaters_wins_a_generation() {
    let indexer_dir = tempfile::tempdir().unwrap();
    let indexer = open(URL, indexer_dir.path());
    for (k, repo) in REPOS.iter().enumerate() {
        let file = format!("src/m{k}.py");
        let src = format!("def handler_{k}(req):\n    return req\n");
        let meta = write(indexer_dir.path(), repo, &commit(k), &[(&file, &src)]);
        publish_snapshot(&indexer, repo, "main", RefKind::Default, &meta.id, &Retention::default()).unwrap();
    }

    // Generation 1: the core repository alone.
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let base = open(URL, dirs[0].path());
    assert_eq!(rebuild(&base, &REPOS[..1]), 1);
    assert!(publish_generation(&base).unwrap().published);

    // Two updaters pull generation 1 and each adds another repository.
    let updaters: Vec<ServiceStore> = dirs[1..].iter().map(|d| open(URL, d.path())).collect();
    for (u, extra) in updaters.iter().zip([REPOS[1], REPOS[2]]) {
        pull_generation(u, None).unwrap().unwrap();
        assert_eq!(rebuild(u, &[REPOS[0], extra]), 2);
    }
    let barrier = Barrier::new(2);
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = updaters
            .iter()
            .map(|u| {
                let barrier = &barrier;
                s.spawn(move || {
                    barrier.wait();
                    publish_generation(u)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners: Vec<usize> = (0..2).filter(|&i| results[i].as_ref().is_ok_and(|r| r.published)).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    let (winner, loser) = (winners[0], 1 - winners[0]);
    let err = results[loser].as_ref().unwrap_err();
    let conflict = err.downcast_ref::<Conflict>().unwrap_or_else(|| panic!("not a Conflict: {err:#}"));
    assert_eq!(conflict.generation, 2);
    eprintln!("generation race: updater {winner} won, updater {loser}: {conflict}");

    // Publishing the winner's generation again is a no-op.
    let again = publish_generation(&updaters[winner]).unwrap();
    assert!(!again.published && again.generation == 2);

    // The loser pulls the winner's generation, rebuilds on top of it and
    // publishes generation 3 with all three repositories.
    let loser = &updaters[loser];
    pull_generation(loser, None).unwrap().unwrap();
    assert_eq!(rebuild(loser, &REPOS), 3);
    let third = publish_generation(loser).unwrap();
    assert!(third.published && third.base == Some(2));

    // A replica sees generation 3 with every repository.
    let replica_dir = tempfile::tempdir().unwrap();
    let replica = open(URL, replica_dir.path());
    assert_eq!(pull_generation(&replica, None).unwrap().unwrap().generation, 3);
    let g = current_generation(&replica.local.dir, "acme").unwrap().unwrap();
    let mut repos: Vec<&str> = g.repos.iter().map(|r| r.repo.as_str()).collect();
    repos.sort_unstable();
    let mut want = REPOS.to_vec();
    want.sort_unstable();
    assert_eq!(repos, want);
    // Roots are the replica's synthetic roots.
    for r in &g.repos {
        assert_eq!(r.default.root, replica.mirror_root(&r.repo, None).to_string_lossy());
    }
}
