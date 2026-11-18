//! Principal → visibility (RFC 0008 §6.4): the repository-id globs a
//! principal was granted, matched against the current generation's
//! repository ids, cached per (glob set, generation). No glob, or no
//! match: `Visibility::Repos(∅)` (deny by default).
//!
//! Globs use `globset` with `literal_separator`: `*` stays within one
//! path component (`github.com/acme/*` matches `github.com/acme/billing`,
//! not `github.com/acme/billing/web`), `**` crosses them.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use globset::{GlobBuilder, GlobMatcher, GlobSetBuilder};
use rueca_query::Visibility;

/// Compiles one repository glob.
pub fn glob(pattern: &str) -> Result<GlobMatcher, globset::Error> {
    Ok(GlobBuilder::new(pattern).literal_separator(true).build()?.compile_matcher())
}

/// The repository ids among `repos` that any of `globs` matches.
pub fn matching(globs: &[String], repos: &[String]) -> BTreeSet<String> {
    let mut set = GlobSetBuilder::new();
    for g in globs {
        // Invalid globs were rejected when the configuration was loaded.
        if let Ok(compiled) = GlobBuilder::new(g).literal_separator(true).build() {
            set.add(compiled);
        }
    }
    match set.build() {
        Ok(set) => repos.iter().filter(|r| set.is_match(r.as_str())).cloned().collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Resolved visibilities of the current generation.
#[derive(Default)]
pub struct VisibilityCache {
    inner: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    generation: u64,
    sets: HashMap<Vec<String>, Arc<BTreeSet<String>>>,
}

impl VisibilityCache {
    /// The visibility of a principal granted `globs`, at `generation`
    /// whose repositories are `repos`.
    pub fn resolve(&self, globs: &[String], generation: u64, repos: &[String]) -> Visibility {
        if globs.is_empty() {
            return Visibility::Repos(BTreeSet::new());
        }
        let mut key = globs.to_vec();
        key.sort();
        key.dedup();
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if st.generation != generation {
            st.generation = generation;
            st.sets.clear();
        }
        let set = st.sets.entry(key).or_insert_with_key(|k| Arc::new(matching(k, repos))).clone();
        Visibility::Repos(set.as_ref().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn globs_match_repository_ids() {
        let repos =
            ids(&["github.com/acme/billing", "github.com/acme/core", "github.com/other/x", "gitlab.com/acme/a/b"]);
        let got = |g: &[&str]| matching(&ids(g), &repos).into_iter().collect::<Vec<_>>();
        assert_eq!(got(&["github.com/acme/*"]), ["github.com/acme/billing", "github.com/acme/core"]);
        assert_eq!(got(&["github.com/acme/billing"]), ["github.com/acme/billing"]);
        assert_eq!(got(&["gitlab.com/acme/*"]), Vec::<String>::new());
        assert_eq!(got(&["gitlab.com/**"]), ["gitlab.com/acme/a/b"]);
        assert_eq!(got(&[]), Vec::<String>::new());
        assert!(glob("[").is_err());
    }

    #[test]
    fn deny_by_default_and_cached_per_generation() {
        let cache = VisibilityCache::default();
        let none = cache.resolve(&[], 1, &ids(&["a/b"]));
        assert_eq!(none, Visibility::Repos(BTreeSet::new()));
        let v1 = cache.resolve(&ids(&["a/*"]), 1, &ids(&["a/b"]));
        assert!(v1.allows("a/b"));
        // Same generation: the cached set, even if the list passed differs.
        assert_eq!(cache.resolve(&ids(&["a/*"]), 1, &ids(&["a/b", "a/c"])), v1);
        let v2 = cache.resolve(&ids(&["a/*"]), 2, &ids(&["a/b", "a/c"]));
        assert!(v2.allows("a/c"));
    }
}
