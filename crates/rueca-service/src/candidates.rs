//! Two-round deletion (`gc/candidates.json`): garbage collection deletes an
//! object only after it was found unreferenced in two collections at least
//! `grace` apart, and publishers take back the objects they reuse.
//!
//! Protocol (crate docs, *Garbage collection*):
//!
//! - A collection records each unreferenced immutable object older than
//!   the grace as a **candidate** with the time it was first seen; an
//!   object already a candidate keeps its time; a candidate that is live
//!   again (or gone) leaves the list.
//! - A candidate seen unreferenced again, `grace` or more after it was
//!   first seen, is **committed**: one CAS of the file moves it from
//!   `candidates` to `deleting`, but only if the fresh file still has it
//!   with the same time (a publisher may have taken it back). Then the
//!   collection deletes everything in `deleting` and removes it from the
//!   list (a crashed collection's `deleting` entries are finished by the
//!   next one).
//! - A publisher, before its uploads and catalog CAS, **rescues** every
//!   object its snapshot (or generation) names: one CAS removing them from
//!   `candidates`. An object in `deleting` cannot be rescued: the
//!   publisher waits until the collection deleted it (and cleared the
//!   entry), then uploads it again.
//!
//! Why this closes the race: an object a publisher rescued at `t` is
//! re-recorded at the earliest by a collection that marks after `t`, with
//! that collection's time, so it can be committed only by a collection
//! that marks at least `grace` after `t`; the publisher's catalog CAS (well
//! within `grace` of `t`) has landed by then and that mark sees it live. A
//! commit that wins the CAS before the rescue makes the publisher wait and
//! re-upload. **Assumption: a publish completes (rescue to catalog CAS)
//! within the grace period.**

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use anyhow::bail;
use rueca_storage::{CasStep, ObjectMeta, Storage, cas_update};
use serde::{Deserialize, Serialize};

use crate::CAS_ATTEMPTS;

/// The candidate list, at the root of the store.
pub(crate) const CANDIDATES_FILE: &str = "gc/candidates.json";

/// How long a publisher waits for a collection to finish deleting an
/// object it needs before it fails.
const RESCUE_WAIT: Duration = Duration::from_secs(60);
const RESCUE_PAUSE: Duration = Duration::from_millis(100);

/// `gc/candidates.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Candidates {
    /// Unreferenced objects → when a collection first saw them so (unix
    /// seconds).
    #[serde(default)]
    pub candidates: BTreeMap<String, u64>,
    /// Committed for deletion: being deleted by a collection.
    #[serde(default)]
    pub deleting: BTreeSet<String>,
}

/// Takes `paths` out of the candidate list before a publish references
/// them; waits while any is being deleted (it is then gone and the caller
/// uploads it again).
pub(crate) fn rescue(storage: &dyn Storage, paths: &[String]) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        let out = cas_update(storage, CANDIDATES_FILE, CAS_ATTEMPTS, |cur: Option<Candidates>| {
            let Some(mut c) = cur else { return Ok(CasStep::Unchanged(Candidates::default())) };
            if paths.iter().any(|p| c.deleting.contains(p)) {
                return Ok(CasStep::Unchanged(c));
            }
            let before = c.candidates.len();
            c.candidates.retain(|p, _| !paths.contains(p));
            Ok(if c.candidates.len() == before { CasStep::Unchanged(c) } else { CasStep::Write(c) })
        })?;
        let busy: Vec<&String> = paths.iter().filter(|p| out.value.deleting.contains(*p)).collect();
        if busy.is_empty() {
            return Ok(());
        }
        if started.elapsed() >= RESCUE_WAIT {
            bail!("objects are being deleted by a garbage collection that did not finish: {busy:?}; retry later");
        }
        std::thread::sleep(RESCUE_PAUSE);
    }
}

/// One collection's view of the unreferenced objects older than the grace.
pub(crate) struct Round<'a> {
    pub storage: &'a dyn Storage,
    pub now: u64,
    pub grace: u64,
}

impl Round<'_> {
    /// Records `unreferenced` (objects unmarked and older than the grace)
    /// as candidates, commits those first seen at least `grace` ago, deletes
    /// every committed object and clears it. Returns the deleted paths.
    pub fn run(&self, unreferenced: &[&ObjectMeta]) -> anyhow::Result<Vec<String>> {
        let seen = match self.storage.get(CANDIDATES_FILE)? {
            Some(v) => serde_json::from_slice::<Candidates>(&v.value)?,
            None => Candidates::default(),
        };
        let due = |p: &str| seen.candidates.get(p).filter(|t| t.saturating_add(self.grace) <= self.now).copied();
        let doomed: BTreeMap<String, u64> =
            unreferenced.iter().filter_map(|o| due(&o.path).map(|t| (o.path.clone(), t))).collect();
        let keep: Vec<&str> =
            unreferenced.iter().map(|o| o.path.as_str()).filter(|p| !doomed.contains_key(*p)).collect();
        let out = cas_update(self.storage, CANDIDATES_FILE, CAS_ATTEMPTS, |cur: Option<Candidates>| {
            let cur = cur.unwrap_or_default();
            let next = self.next_list(&cur, &doomed, &keep);
            Ok(if next == cur { CasStep::Unchanged(cur) } else { CasStep::Write(next) })
        })?;
        let deleting: Vec<String> = out.value.deleting.iter().cloned().collect();
        for p in &deleting {
            self.storage.delete(p)?;
        }
        if !deleting.is_empty() {
            cas_update(self.storage, CANDIDATES_FILE, CAS_ATTEMPTS, |cur: Option<Candidates>| {
                let mut c = cur.unwrap_or_default();
                c.deleting.retain(|p| !deleting.contains(p));
                Ok(CasStep::Write(c))
            })?;
        }
        Ok(deleting)
    }

    /// The new list from the fresh one `cur`: a doomed object still listed
    /// with the time this round saw moves to `deleting` (otherwise a
    /// publisher took it back); every other unreferenced object is a
    /// candidate, keeping its time when `cur` has it (a rescued one starts
    /// over at `now`). Candidates not unreferenced now are dropped.
    fn next_list(&self, cur: &Candidates, doomed: &BTreeMap<String, u64>, keep: &[&str]) -> Candidates {
        let mut next = Candidates { candidates: BTreeMap::new(), deleting: cur.deleting.clone() };
        for (p, t) in doomed {
            if cur.candidates.get(p) == Some(t) {
                next.deleting.insert(p.clone());
            } else {
                next.candidates.insert(p.clone(), self.now);
            }
        }
        for p in keep {
            next.candidates.insert((*p).to_string(), cur.candidates.get(*p).copied().unwrap_or(self.now));
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(now: u64) -> Round<'static> {
        static NONE: std::sync::OnceLock<rueca_storage::MemoryStorage> = std::sync::OnceLock::new();
        Round { storage: NONE.get_or_init(rueca_storage::MemoryStorage::new), now, grace: 10 }
    }

    #[test]
    fn rescued_candidates_start_over() {
        let cur = Candidates {
            candidates: [("a".to_string(), 100), ("b".to_string(), 100), ("gone".to_string(), 1)].into(),
            deleting: BTreeSet::new(),
        };
        // `a` is still listed with the time seen: committed. `c` was
        // rescued after the round read the list: a new candidate.
        let doomed: BTreeMap<String, u64> = [("a".to_string(), 100), ("c".to_string(), 100)].into();
        let next = round(200).next_list(&cur, &doomed, &["b", "d"]);
        assert_eq!(next.deleting, ["a".to_string()].into());
        assert_eq!(next.candidates, [("b".to_string(), 100), ("c".to_string(), 200), ("d".to_string(), 200)].into());
    }
}
