//! Service stores (RFC 0008 phase 0): `rueca status --store-url`, and the
//! experimental, hidden `rueca publish` and `rueca pull`.
//!
//! ```text
//! rueca status --store-url URL [--store-option K=V]... [--store DIR] [--json]
//! rueca publish [PATH | --repo ID] [--ref NAME] --store-url URL [--org NAME] [--store DIR] [--json]
//! rueca pull [--repo ID | --generation] --store-url URL [--store DIR] [--json]
//! ```
//!
//! `--store` is the local store: where `publish` reads the snapshot and
//! where `pull` mirrors the service store (`rueca query --store DIR` then
//! reads it). `publish` creates the service store for `--org` when it is
//! new; the other commands read the organization from its `store.json`.
//! `pull --generation` then builds the organization from the mirror and
//! publishes it as the store's next generation, what phase 2's updater
//! will do (until then, the way to give `rueca serve` something to serve).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use rueca_segment::{CatalogEntry, Store};
use rueca_service::{
    PullWhich, RefKind, Retention, ServiceStatus, ServiceStore, publish_generation, publish_snapshot, pull_generation,
    pull_repo, status,
};
use serde_json::json;

use crate::{Output, human_age, human_bytes, render, short};

/// The service store a command talks to.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct StoreArgs {
    /// Service store URL: `memory://NAME`, `file:///abs/dir`,
    /// `s3://bucket/prefix`, `gs://bucket/prefix` (RFC 0008).
    #[arg(long, value_name = "URL")]
    pub store_url: Option<String>,
    /// Backend option `KEY=VALUE` (repeatable), e.g. `region=eu-west-1`,
    /// `endpoint=http://localhost:9000`.
    #[arg(long = "store-option", value_name = "KEY=VALUE", requires = "store_url")]
    pub store_option: Vec<String>,
}

impl StoreArgs {
    fn url(&self) -> anyhow::Result<&str> {
        self.store_url.as_deref().context("--store-url <URL> is required")
    }

    fn options(&self) -> anyhow::Result<BTreeMap<String, String>> {
        self.store_option
            .iter()
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
                _ => bail!("--store-option `{kv}`: expected KEY=VALUE"),
            })
            .collect()
    }

    /// Opens the store for `org` (created when new) or, without `org`, the
    /// existing store's organization; `local` is the mirror.
    fn open(&self, local: &Path, org: Option<&str>) -> anyhow::Result<ServiceStore> {
        match org {
            Some(org) => ServiceStore::open(self.url()?, &self.options()?, local, org),
            None => ServiceStore::connect(self.url()?, &self.options()?, local),
        }
    }
}

/// `rueca status --store-url`: repositories, refs, generation, bytes.
pub fn cmd_status(args: &StoreArgs, local: &Path, out: Output) -> anyhow::Result<()> {
    let svc = args.open(local, None)?;
    let st = status(&svc)?;
    if out.json {
        let mut v = serde_json::to_value(&st)?;
        v["store_url"] = json!(args.url()?);
        println!("{}", render(&v, out.pretty));
        return Ok(());
    }
    print_status(args.url()?, &st);
    Ok(())
}

fn print_status(url: &str, st: &ServiceStatus) {
    let generation = st.generation.map_or_else(|| "no generation".to_string(), |g| format!("generation {g}"));
    println!("{url}: organization {} ({generation}), {} objects, {}", st.org, st.objects, human_bytes(st.bytes));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut rows = vec![["REPO", "REF", "KIND", "SNAPSHOTS", "COMMIT", "AGE", "CLOSED"].map(str::to_string)];
    for r in &st.repos {
        for (i, x) in r.refs.iter().enumerate() {
            let repo = if i == 0 { r.repo.clone() } else { String::new() };
            let age = |t: Option<u64>| t.map_or_else(String::new, |t| human_age(now.saturating_sub(t)));
            let commit = x.commit.as_deref().map_or("", short).to_string();
            let kind = x.kind.as_str().into();
            let (indexed, closed) = (age(x.indexed_at), age(x.closed_at));
            rows.push([repo, x.ref_name.clone(), kind, x.snapshots.to_string(), commit, indexed, closed]);
        }
    }
    print_table(&rows);
    println!();
    let mut rows = vec![["CLASS", "OBJECTS", "BYTES"].map(str::to_string)];
    for (class, c) in &st.classes {
        rows.push([class.clone(), c.objects.to_string(), human_bytes(c.bytes)]);
    }
    print_table(&rows);
}

fn print_table<const N: usize>(rows: &[[String; N]]) {
    let mut widths = [0usize; N];
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for row in rows {
        let cells: Vec<String> = row.iter().zip(widths).map(|(c, w)| format!("{c:<w$}")).collect();
        println!("{}", cells.join("  ").trim_end());
    }
}

/// `rueca publish`: a snapshot of the local store to the service store.
pub struct PublishCmd {
    pub path: Option<PathBuf>,
    pub repo: Option<String>,
    pub ref_name: Option<String>,
}

pub fn cmd_publish(
    args: &StoreArgs,
    store: &Path,
    org: Option<&str>,
    cmd: PublishCmd,
    out: Output,
) -> anyhow::Result<()> {
    let local = Store::open(store)?;
    let entry = local_entry(&local, &cmd)?;
    let svc = args.open(store, org)?;
    let (ref_name, kind) = match &cmd.ref_name {
        Some(r) => (r.clone(), ref_kind(r)),
        None => {
            let seg = local.open_segment(&entry.segment_id)?;
            (seg.snapshot().branch.clone().unwrap_or_else(|| "HEAD".to_string()), RefKind::Default)
        }
    };
    let report = publish_snapshot(&svc, &entry.repo, &ref_name, kind, &entry.segment_id, &Retention::default())?;
    if out.json {
        println!("{}", render(&serde_json::to_value(&report)?, out.pretty));
    } else if report.duplicate {
        println!("{}@{} ({}) is already published", report.repo, report.ref_name, short(&report.commit));
    } else {
        println!(
            "published {}@{} ({}): segment {}, {} packs uploaded ({} present), {}",
            report.repo,
            report.ref_name,
            short(&report.commit),
            report.segment,
            report.packs_uploaded,
            report.packs_present,
            human_bytes(report.bytes_uploaded)
        );
    }
    Ok(())
}

/// The local catalog entry `publish` names: a ref snapshot with `--ref`,
/// else a working tree (by `--repo`, else the root containing PATH).
fn local_entry(local: &Store, cmd: &PublishCmd) -> anyhow::Result<CatalogEntry> {
    let by_path = || -> anyhow::Result<CatalogEntry> {
        let path = cmd.path.clone().unwrap_or_else(|| PathBuf::from("."));
        local.entry_for_path(&path)?.with_context(|| format!("`{}` is not indexed in this store", path.display()))
    };
    let repo = match &cmd.repo {
        Some(r) => r.clone(),
        None if cmd.ref_name.is_none() => return by_path(),
        None => by_path()?.repo,
    };
    let mut hits: Vec<CatalogEntry> = local
        .entries()?
        .into_iter()
        .filter(|e| e.repo == repo && e.ref_name.as_deref() == cmd.ref_name.as_deref())
        .collect();
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => bail!(
            "no snapshot of `{repo}` at ref `{}` in this store",
            cmd.ref_name.as_deref().unwrap_or("(working tree)")
        ),
        _ => bail!("`{repo}` is indexed at several roots; pass PATH instead of --repo"),
    }
}

/// `pr/<n>` → pull request; a hex string of at least 7 digits → commit;
/// anything else → branch (tags are published as branches by this
/// command).
fn ref_kind(name: &str) -> RefKind {
    let hex = name.len() >= 7 && name.bytes().all(|b| b.is_ascii_hexdigit());
    match name.strip_prefix("pr/") {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => RefKind::Pr,
        _ if hex => RefKind::Commit,
        _ => RefKind::Branch,
    }
}

/// `rueca pull`: mirror the service store (or one repository) into the
/// local store, then its organization generation; with `publish`, build
/// and publish the next generation from the mirror.
pub fn cmd_pull(args: &StoreArgs, store: &Path, repo: Option<&str>, publish: bool, out: Output) -> anyhow::Result<()> {
    let svc = args.open(store, None)?;
    let repos: Vec<String> = match repo {
        Some(r) => vec![r.to_string()],
        None => status(&svc)?.repos.into_iter().map(|r| r.repo).collect(),
    };
    let mut reports = Vec::new();
    for r in &repos {
        reports.push(pull_repo(&svc, r, PullWhich::All)?);
    }
    let generation = if repo.is_none() { pull_generation(&svc, None)? } else { None };
    let published = if publish {
        rueca_segment::org::update_org(&svc.local, &svc.org, &svc.mirror_members()?)?;
        Some(publish_generation(&svc)?)
    } else {
        None
    };
    if out.json {
        let v = json!({ "repos": reports, "generation": generation, "published": published });
        println!("{}", render(&v, out.pretty));
        return Ok(());
    }
    for r in &reports {
        let refs: Vec<String> = r.refs.iter().map(|x| format!("{}@{}", x.ref_name, short(&x.commit))).collect();
        println!(
            "pulled {}: {} ({} segments, {} packs, {})",
            r.repo,
            refs.join(", "),
            r.segments_downloaded,
            r.packs_downloaded,
            human_bytes(r.bytes_downloaded)
        );
    }
    if let Some(g) = generation {
        println!("pulled organization {} generation {} ({} repositories)", g.org, g.generation, g.repos);
    }
    if let Some(p) = published {
        println!("published organization {} generation {} ({} files uploaded)", p.org, p.generation, p.files_uploaded);
    }
    println!("mirror: {}", svc.local.dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_kinds() {
        assert_eq!(ref_kind("pr/12"), RefKind::Pr);
        assert_eq!(ref_kind("pr/x"), RefKind::Branch);
        assert_eq!(ref_kind("7696364aef89"), RefKind::Commit);
        assert_eq!(ref_kind("release/2.4"), RefKind::Branch);
        assert_eq!(ref_kind("cafe"), RefKind::Branch);
    }

    #[test]
    fn store_options() {
        let a = StoreArgs { store_url: Some("s3://b".into()), store_option: vec!["region=x".into(), "k==v".into()] };
        let o = a.options().unwrap();
        assert_eq!(o["region"], "x");
        assert_eq!(o["k"], "=v");
        let bad = StoreArgs { store_url: None, store_option: vec!["novalue".into()] };
        assert!(bad.options().is_err());
        assert!(bad.url().is_err());
    }
}
