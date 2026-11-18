//! What a service store holds (`rueca status --store-url`).

use std::collections::BTreeMap;

use anyhow::Context;
use rueca_storage::Storage;
use serde::Serialize;

use crate::catalog::{RefKind, RepoCatalog};
use crate::generation::{OrgPointer, POINTER_FILE};
use crate::{ServiceStore, StoreInfo, read_store_info};

/// The store's repositories, refs and generation, and its bytes by class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ServiceStatus {
    /// `store.json` (`None`: not created yet).
    pub store: Option<StoreInfo>,
    pub org: String,
    /// Current generation of the organization (`None`: none published).
    pub generation: Option<u64>,
    pub repos: Vec<RepoStatus>,
    /// Objects and bytes per class: `catalogs`, `segments`, `packs`,
    /// `pointers`, `generations`, `org_index`, `profiles`, `links`, `other`.
    pub classes: BTreeMap<String, ClassUsage>,
    pub objects: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ClassUsage {
    pub objects: u64,
    pub bytes: u64,
}

/// One repository catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoStatus {
    pub repo: String,
    pub repo_dir: String,
    pub default_ref: Option<String>,
    pub refs: Vec<RefStatus>,
    /// Refs expired or removed so far (kept in the catalog).
    pub removed: usize,
}

/// One ref: its head snapshot and how many snapshots it keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefStatus {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub kind: RefKind,
    pub snapshots: usize,
    pub commit: Option<String>,
    pub segment: Option<String>,
    pub indexed_at: Option<u64>,
    pub closed_at: Option<u64>,
}

/// Reads the store's catalogs and pointer and classifies every object.
pub fn status(svc: &ServiceStore) -> anyhow::Result<ServiceStatus> {
    let storage = svc.storage.as_ref();
    let mut st = ServiceStatus { store: read_store_info(storage)?, org: svc.org.clone(), ..ServiceStatus::default() };
    for o in storage.list("")? {
        let class = object_class(&o.path);
        let c = st.classes.entry(class.to_string()).or_default();
        c.objects += 1;
        c.bytes += o.size;
        st.objects += 1;
        st.bytes += o.size;
        if class == "catalogs" {
            st.repos.push(repo_status(storage, &o.path)?);
        }
    }
    if let Some(v) = storage.get(&svc.org_path(POINTER_FILE))? {
        let ptr: OrgPointer = serde_json::from_slice(&v.value).context("org.json is not valid")?;
        st.generation = Some(ptr.generation);
    }
    st.repos.sort_by(|a, b| a.repo.cmp(&b.repo));
    Ok(st)
}

/// The class of an object path (`/status`, `rueca status`, garbage
/// collection metrics).
pub fn object_class(path: &str) -> &'static str {
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["repos", _, "catalog.json"] => "catalogs",
        ["repos", _, "segments", _] => "segments",
        ["repos", _, "packs", _] => "packs",
        ["orgs", _, POINTER_FILE] => "pointers",
        ["orgs", _, "generations", _] => "generations",
        ["orgs", _, "index", _] => "org_index",
        ["orgs", _, "profiles", _] => "profiles",
        ["orgs", _, "links", _] => "links",
        ["jobs", _] => "jobs",
        ["leases", _] => "leases",
        ["reads", ..] => "reads",
        _ => "other",
    }
}

fn repo_status(storage: &dyn Storage, path: &str) -> anyhow::Result<RepoStatus> {
    let v = storage.get(path)?.with_context(|| format!("{path} vanished"))?;
    let c: RepoCatalog = serde_json::from_slice(&v.value).with_context(|| format!("{path} is not valid"))?;
    let refs = c
        .refs
        .iter()
        .map(|(name, r)| {
            let head = r.snapshots.first();
            RefStatus {
                ref_name: name.clone(),
                kind: r.kind,
                snapshots: r.snapshots.len(),
                commit: head.map(|s| s.commit.clone()),
                segment: head.map(|s| s.segment.clone()),
                indexed_at: head.map(|s| s.indexed_at),
                closed_at: r.closed_at,
            }
        })
        .collect();
    let repo_dir = path.trim_start_matches("repos/").trim_end_matches("/catalog.json").to_string();
    Ok(RepoStatus { repo: c.repo, repo_dir, default_ref: c.default_ref, refs, removed: c.removed.len() })
}
