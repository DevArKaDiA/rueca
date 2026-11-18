//! `rueca` — the Rueca command-line interface. See `docs/tools.md` for the
//! contract.
//!
//! ```text
//! rueca index [PATH] [--store DIR] [--repo ID] [--json]
//! rueca index [PATH] --ref NAME [--remove] [--store DIR] [--repo ID] [--json]
//! rueca index --org FILE [PATH] [--store DIR] [--json]
//! rueca index --org FILE PATH --ref NAME [--remove] [--store DIR] [--json]
//! rueca query <TOOL> '<JSON ARGS>' [--root PATH | --org FILE|NAME] [--store DIR] [--pretty] [--text]
//! rueca mcp [--root PATH | --org FILE|NAME] [--store DIR] [--no-auto-index]
//! rueca status [--store DIR] [--json]
//! rueca status --store-url URL [--store-option KEY=VALUE]... [--json]
//! rueca serve [--config FILE] [--role query|indexer|all] [--check-config]
//! rueca links --org FILE|NAME [--repo ID [--ref NAME]] [--store DIR] [--json]
//! rueca contracts [--org FILE|NAME | --root PATH] [--repo ID [--ref NAME]] [--side provided|consumed|dangling]
//!                 [--kind http|rpc|topic] [--query TEXT] [--limit N] [--store DIR] [--json | --text]
//! ```
//!
//! `--store`, `--root`, `--org`, `-v`, `--json` and `--pretty` are global
//! (accepted before or after the subcommand). `--org` names an organization
//! (RFC 0006): its file (`rueca-org.toml`) or, for `query` and `mcp`, the
//! name of an organization already built in the store. `index --org FILE`
//! indexes every repository of the file (or only the one containing PATH)
//! and updates the organization; a plain `index PATH` of a member also
//! updates the organizations that hold it. `index PATH --ref NAME` indexes a
//! branch, `pr/<n>`, tag or commit of PATH's repository as a snapshot of its
//! own, exported under `<store>/refs/` (RFC 0006 §9.3; `--remove` drops it)
//! and updates the organizations holding the repository; `index --org FILE
//! PATH --ref NAME` does it for one member of the file (PATH: its path, `id`
//! or `url`), and `index --org FILE` syncs every member's `refs`. `status`
//! lists ref snapshots under their repository and each organization's refs;
//! `links --ref` prints one ref entry's link set; contract edges are marked
//! (`via: "contract"`, RFC 0006 phase 4). `contracts` lists the contract
//! tables (the `list_contracts` tool, every page: `--limit` caps the
//! entries) of an organization or, with `--root`, of a local root. JSON
//! output is compact unless `--pretty`.
//! Logs go to stderr (`-v` debug, `-vv` trace, or `RUECA_LOG=<filter>`).
//! `query` prints the tool's JSON result to stdout (with `--text`, the
//! compact text rendering an MCP client gets as text content); on error it
//! prints `{"error": "..."}` and exits with status 2. Other commands report errors on
//! stderr and exit with status 1.
//!
//! `status --store-url` reports a service store (RFC 0008: object storage
//! or a `file://` directory) instead of the local store: its repositories,
//! refs, organization generation and bytes by object class. The hidden,
//! experimental `publish` and `pull` commands move snapshots between the
//! local store and a service store (see `service.rs`).
//!
//! `serve` runs the service mode (RFC 0008, crate `rueca-serve`, feature
//! `service`, on by default): the query role serves MCP over HTTP from a
//! service store; the indexer role (`--role indexer`, or `all` for both)
//! polls the `[[repo]]` repositories, leases and runs index jobs and, on
//! the leader, publishes organization generations. It indexes each ref in
//! a child process of this binary, the hidden `rueca index-worker` (one
//! JSON request on stdin, the report on stdout, JSON logs on stderr;
//! `rueca_service::worker`). Configured by `rueca-service.toml` (`--config`, else
//! `RUECA_CONFIG`); `--check-config` validates the file, its `env:`
//! references and the store, prints the problems and exits 0 or 1. Its logs
//! go to stdout as `[telemetry]` says (JSON by default), filtered by
//! `RUECA_LOG` (else `-v`, else `info`).
//!
//! `query` and `mcp` default to the indexed root that contains the current
//! directory when `--root` is not given. `mcp` indexes `--root` (in the
//! background) when it has no segment yet; without an explicit `--root`
//! nothing is indexed automatically, so launching the server from an
//! arbitrary directory (e.g. `$HOME`) never starts a huge index.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

mod contracts;
mod serve;
mod service;

use clap::{Parser, Subcommand};
use rueca_segment::org::OrgGeneration;
use rueca_segment::{CatalogEntry, Store};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "rueca",
    version,
    about = "Code intelligence for coding agents: index a working tree, query it, serve it over MCP."
)]
struct Cli {
    /// Store directory (default: $RUECA_HOME, else ~/.cache/rueca).
    #[arg(long, global = true, value_name = "DIR")]
    store: Option<PathBuf>,
    /// Working-tree root used by default by `query` and `mcp`
    /// (default: the indexed root containing the current directory).
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,
    /// Organization: its file (`rueca-org.toml`) or, for `query` / `mcp`,
    /// the name of an organization built in the store.
    #[arg(long, global = true, value_name = "FILE|NAME")]
    org: Option<String>,
    /// More logging on stderr (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Machine-readable JSON output (`index`, `status`; `query` always prints JSON).
    #[arg(long, global = true)]
    json: bool,
    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pretty: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Index the working tree at PATH (default: current directory); with
    /// --org FILE, every repository of the organization (or the one
    /// containing PATH) and then the organization layer.
    Index {
        /// Working tree to index (with --org: only this member).
        path: Option<PathBuf>,
        /// Repository id override (`github.com/owner/name`, `local/name`).
        #[arg(long, value_name = "ID")]
        repo: Option<String>,
        /// Worker threads (default: available parallelism).
        #[arg(long, value_name = "N")]
        threads: Option<usize>,
        /// Index a ref of PATH's repository instead of its working tree: a
        /// branch, `pr/<n>`, a tag or a commit (fetched from `origin` when
        /// absent), exported under the store as a snapshot of its own; the
        /// organizations holding the repository list it. With --org FILE:
        /// a ref of the member PATH (its path, `id` or `url`).
        #[arg(long = "ref", value_name = "NAME")]
        ref_name: Option<String>,
        /// With --ref: remove that ref snapshot and its export.
        #[arg(long, requires = "ref_name")]
        remove: bool,
    },
    /// Run one tool and print its JSON result. Same semantics as MCP.
    #[command(
        after_help = "Tools: search_symbols, search_code, get_symbol, get_snippet, trace, get_file_outline, list_repositories, \
list_contracts\n\
Example: rueca query trace '{\"qualified_name\": \"InvoiceService.issue\", \"direction\": \"callers\"}'"
    )]
    Query {
        /// Tool name.
        tool: String,
        /// Tool arguments as a JSON object (`-` reads stdin). Default `{}`.
        args: Option<String>,
        /// Print the compact text rendering (the MCP text content) instead of JSON.
        #[arg(long)]
        text: bool,
    },
    /// Serve MCP on stdio. Indexes --root first if it has no segment.
    Mcp {
        /// Do not index --root automatically.
        #[arg(long)]
        no_auto_index: bool,
    },
    /// List indexed roots with repo, commit, node/edge/file counts and age,
    /// and the organizations of the store; with --store-url, what a service
    /// store holds.
    Status {
        #[command(flatten)]
        remote: service::StoreArgs,
    },
    /// Service mode (RFC 0008): serve MCP over HTTP from a service store
    /// (query role) and/or keep it indexed (indexer role).
    /// Configuration: `rueca-service.toml` (--config, else $RUECA_CONFIG).
    Serve {
        /// The service configuration file (`rueca-service.toml`).
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
        /// `query` (default, else $RUECA_ROLE), `indexer` or `all` (both in
        /// one process).
        #[arg(long, value_name = "ROLE")]
        role: Option<String>,
        /// Validate the configuration, its secrets and the store, print the
        /// problems, exit 0 (valid) or 1.
        #[arg(long)]
        check_config: bool,
    },
    /// Experimental (RFC 0008): publish a snapshot of the local store (the
    /// working tree containing PATH, or --repo; with --ref, that ref
    /// snapshot) to the service store --store-url. --org creates the store
    /// for that organization when it is new.
    #[command(hide = true)]
    Publish {
        path: Option<PathBuf>,
        #[arg(long, value_name = "ID")]
        repo: Option<String>,
        #[arg(long = "ref", value_name = "NAME")]
        ref_name: Option<String>,
        #[command(flatten)]
        remote: service::StoreArgs,
    },
    /// Experimental (RFC 0008): mirror the service store --store-url (every
    /// repository, or --repo) and its organization generation into the
    /// local store. With --generation, then build the organization from the
    /// mirror and publish it as the store's next generation (the job of
    /// phase 2's updater; use a store directory dedicated to the mirror).
    #[command(hide = true)]
    Pull {
        #[arg(long, value_name = "ID")]
        repo: Option<String>,
        #[arg(long, conflicts_with = "repo")]
        generation: bool,
        #[command(flatten)]
        remote: service::StoreArgs,
    },
    /// Service mode (RFC 0008 §7.8), internal: the indexer's child process.
    /// Reads one index request (JSON) on stdin, writes `{"ok": <report>}`
    /// or `{"error": …}` on stdout, logs JSON lines on stderr.
    #[command(name = "index-worker", hide = true)]
    IndexWorker,
    /// List the cross-repository edges of an organization (--org): the link
    /// layer of RFC 0006 (imports of packages published by another
    /// repository, and the calls, inheritance and references through them;
    /// calls to HTTP routes and RPC methods another repository provides).
    Links {
        /// Only edges leaving this repository (id).
        #[arg(long, value_name = "ID")]
        repo: Option<String>,
        /// With --repo: the link set of that ref entry of the repository
        /// (its edges into its dependencies' default refs).
        #[arg(long = "ref", value_name = "NAME", requires = "repo")]
        ref_name: Option<String>,
    },
    /// List the contracts of an organization (--org) or of a local root
    /// (--root): HTTP routes, spec operations and RPC methods provided, the
    /// calls to them, and the calls nothing provides (`--side dangling`).
    Contracts {
        /// Only this repository (id or indexed root path).
        #[arg(long, value_name = "ID")]
        repo: Option<String>,
        /// With --repo: read the repository at this ref.
        #[arg(long = "ref", value_name = "NAME", requires = "repo")]
        ref_name: Option<String>,
        /// `provided`, `consumed` or `dangling` (default: provided and consumed).
        #[arg(long, value_name = "SIDE")]
        side: Option<String>,
        /// `http`, `rpc` or `topic`.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        /// Keep entries whose key, raw text or symbol contains this text.
        #[arg(long, value_name = "TEXT")]
        query: Option<String>,
        /// At most this many entries (default: all).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Print the compact text rendering (the default without --json).
        #[arg(long)]
        text: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Serve { config, role, check_config } = cli.command {
        return serve::cmd_serve(config, role.as_deref(), check_config, cli.verbose);
    }
    if let Command::IndexWorker = cli.command {
        return serve::cmd_index_worker();
    }
    init_logging(cli.verbose, matches!(cli.command, Command::Mcp { .. }));
    let store = cli.store.clone().unwrap_or_else(Store::default_dir);
    let result = match cli.command {
        Command::Index { path, repo, threads, ref_name, remove } => match (cli.org, ref_name) {
            (Some(org), Some(name)) => {
                let out = Output { json: cli.json, pretty: cli.pretty };
                cmd_index_org_ref(&store, &org, RefCmd { path, repo, name, remove, threads }, out)
            }
            (Some(org), None) => cmd_index_org(&store, &org, path, repo, cli.json, cli.pretty, threads),
            (None, Some(name)) => {
                let out = Output { json: cli.json, pretty: cli.pretty };
                cmd_index_ref(&store, RefCmd { path, repo, name, remove, threads }, out)
            }
            (None, None) => cmd_index(&store, path, repo, cli.json, cli.pretty, threads),
        },
        Command::Query { tool, args, text } => {
            return cmd_query(&store, cli.root, cli.org.as_deref(), &tool, args, cli.pretty, text);
        }
        Command::Mcp { no_auto_index } => cmd_mcp(&store, cli.root, cli.org.as_deref(), !no_auto_index),
        Command::Status { remote } if remote.store_url.is_some() => {
            service::cmd_status(&remote, &store, Output { json: cli.json, pretty: cli.pretty })
        }
        Command::Status { .. } => cmd_status(&store, cli.json, cli.pretty),
        Command::Publish { path, repo, ref_name, remote } => {
            let out = Output { json: cli.json, pretty: cli.pretty };
            let cmd = service::PublishCmd { path, repo, ref_name };
            service::cmd_publish(&remote, &store, cli.org.as_deref(), cmd, out)
        }
        Command::Serve { .. } | Command::IndexWorker => {
            Err(anyhow::anyhow!("`serve` and `index-worker` are dispatched before logging starts"))
        }
        Command::Pull { repo, generation, remote } => {
            let out = Output { json: cli.json, pretty: cli.pretty };
            service::cmd_pull(&remote, &store, repo.as_deref(), generation, out)
        }
        Command::Links { repo, ref_name } => match cli.org.as_deref() {
            Some(org) => {
                let out = Output { json: cli.json, pretty: cli.pretty };
                cmd_links(&store, org, LinksCmd { repo: repo.as_deref(), ref_name: ref_name.as_deref() }, out)
            }
            None => Err(anyhow::anyhow!("`links` needs --org <file|name>")),
        },
        Command::Contracts { repo, ref_name, side, kind, query, limit, text } => {
            let out = Output { json: cli.json && !text, pretty: cli.pretty };
            let filters = contracts::Filters { repo, ref_name, side, kind, query, limit };
            let engine = query_engine(&store, cli.root, cli.org.as_deref());
            engine.and_then(|e| contracts::cmd_contracts(&e, &filters, out.json, out.pretty))
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rueca: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: u8, serving: bool) {
    let default = match verbose {
        0 if serving => "info",
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_env("RUECA_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .try_init();
}

fn cmd_index(
    store: &Path,
    path: Option<PathBuf>,
    repo: Option<String>,
    json: bool,
    pretty: bool,
    threads: Option<usize>,
) -> anyhow::Result<()> {
    let path = path.unwrap_or_else(|| PathBuf::from("."));
    let root = std::fs::canonicalize(&path).map_err(|e| anyhow::anyhow!("cannot open `{}`: {e}", path.display()))?;
    if !root.is_dir() {
        anyhow::bail!("`{}` is not a directory", root.display());
    }
    let opts = rueca_index::IndexOptions {
        root,
        store: store.to_path_buf(),
        repo,
        budget: rueca_extract::Budget::default(),
        threads,
        ..rueca_index::IndexOptions::default()
    };
    let report = rueca_index::index_worktree(&opts)?;
    // Organizations holding this root replace its part (RFC 0006 §3.2).
    let orgs = rueca_index::org::update_orgs_containing(store, Path::new(&report.root))?;
    if json {
        let mut v = serde_json::to_value(&report)?;
        if !orgs.is_empty() {
            v["orgs"] = serde_json::to_value(&orgs)?;
        }
        println!("{}", render(&v, pretty));
        return Ok(());
    }
    let snap = &report.segment.snapshot;
    println!(
        "indexed {} ({} @ {}{})",
        report.root,
        snap.repo,
        short(&snap.commit),
        if snap.dirty { ", dirty" } else { "" }
    );
    print_index_details(&report);
    for u in &orgs {
        print_org_update(u);
    }
    Ok(())
}

/// The files / graph / references / segment lines of an index summary.
fn print_index_details(report: &rueca_index::IndexReport) {
    let s = &report.segment;
    let pct = if report.references_total > 0 {
        100.0 * report.references_resolved as f64 / report.references_total as f64
    } else {
        100.0
    };
    println!(
        "  files:      {} total, {} extracted, {} skipped, {} cache hits",
        report.files_total, report.files_extracted, report.files_skipped, report.facts_cache_hits
    );
    println!("  graph:      {} nodes, {} edges", s.node_count, s.edge_count);
    println!("  references: {}/{} resolved ({pct:.1}%)", report.references_resolved, report.references_total);
    println!("  segment:    {} ({}) in {} ms", s.id, human_bytes(s.bytes), report.duration_ms);
}

/// JSON / pretty flags of a command's output.
#[derive(Debug, Clone, Copy)]
struct Output {
    json: bool,
    pretty: bool,
}

/// `rueca index [PATH] --ref NAME [--remove]`.
struct RefCmd {
    path: Option<PathBuf>,
    repo: Option<String>,
    name: String,
    remove: bool,
    threads: Option<usize>,
}

/// Indexes (or removes) a ref snapshot of PATH's repository (RFC 0006 §9.3).
fn cmd_index_ref(store: &Path, cmd: RefCmd, out: Output) -> anyhow::Result<()> {
    let path = cmd.path.unwrap_or_else(|| PathBuf::from("."));
    let root = std::fs::canonicalize(&path).map_err(|e| anyhow::anyhow!("cannot open `{}`: {e}", path.display()))?;
    if cmd.remove {
        let repo = rueca_index::repo_id(&root, cmd.repo.as_deref());
        let prefix = rueca_index::refs::root_prefix(&root);
        let removed = rueca_index::refs::remove_ref(store, &repo, &prefix, &cmd.name)?;
        // Organizations holding the repository drop the ref's entry.
        let orgs = rueca_index::org::update_orgs_of_repo(store, &repo)?;
        if out.json {
            let mut v = serde_json::to_value(&removed)?;
            if !orgs.is_empty() {
                v["orgs"] = serde_json::to_value(&orgs)?;
            }
            println!("{}", render(&v, out.pretty));
        } else {
            println!("removed ref {} of {} ({})", removed.ref_name, removed.repo, removed.export_dir);
            orgs.iter().for_each(print_org_update);
        }
        return Ok(());
    }
    let report = rueca_index::refs::index_ref(&rueca_index::refs::RefIndexOptions {
        root,
        name: cmd.name,
        store: store.to_path_buf(),
        repo: cmd.repo,
        budget: rueca_extract::Budget::default(),
        threads: cmd.threads,
    })?;
    // Organizations holding the repository list the ref (RFC 0006 §9.3):
    // only its entry changes.
    let orgs = rueca_index::org::update_orgs_of_repo(store, &report.repo)?;
    if out.json {
        let mut v = ref_report_json(&report)?;
        if !orgs.is_empty() {
            v["orgs"] = serde_json::to_value(&orgs)?;
        }
        println!("{}", render(&v, out.pretty));
        return Ok(());
    }
    print_ref_report(&report);
    orgs.iter().for_each(print_org_update);
    Ok(())
}

/// The text summary of an indexed ref.
fn print_ref_report(report: &rueca_index::refs::RefIndexReport) {
    println!(
        "indexed ref {} of {} ({} @ {}){}",
        report.ref_name,
        report.repo,
        report.kind.as_str(),
        short(&report.commit),
        if report.unchanged { ": unchanged" } else { "" }
    );
    if report.unchanged {
        let s = &report.index.segment;
        println!("  export:     {} (kept)", report.export_dir);
        println!("  graph:      {} nodes, {} edges, {} files", s.node_count, s.edge_count, s.file_count);
        println!("  segment:    {} ({})", s.id, human_bytes(s.bytes));
    } else {
        println!("  export:     {} ({} files) in {} ms", report.export_dir, report.export.files, report.export_ms);
        print_index_details(&report.index);
    }
    println!("  total:      {} ms (resolve {} ms)", report.duration_ms, report.resolve_ms);
}

/// The `--json` report of `index --ref`: the export's IndexReport plus the
/// ref fields; `duration_ms` is the whole run, `index_ms` the indexing.
fn ref_report_json(report: &rueca_index::refs::RefIndexReport) -> anyhow::Result<Value> {
    let mut v = serde_json::to_value(&report.index)?;
    v["index_ms"] = json!(report.index.duration_ms);
    for (k, x) in [
        ("ref", json!(report.ref_name)),
        ("kind", serde_json::to_value(report.kind)?),
        ("commit", json!(report.commit)),
        ("repo", json!(report.repo)),
        ("source", json!(report.source)),
        ("fetched", json!(report.fetched)),
        ("export_dir", json!(report.export_dir)),
        ("unchanged", json!(report.unchanged)),
        ("export", serde_json::to_value(&report.export)?),
        ("resolve_ms", json!(report.resolve_ms)),
        ("export_ms", json!(report.export_ms)),
        ("duration_ms", json!(report.duration_ms)),
    ] {
        v[k] = x;
    }
    Ok(v)
}

/// `--org` of `query` / `mcp`: an organization file, or the name of an
/// organization built in the store.
fn org_arg(arg: &str) -> anyhow::Result<(String, Option<rueca_index::org::OrgConfig>)> {
    let p = Path::new(arg);
    if p.is_file() {
        let config = rueca_index::org::OrgConfig::load(p)?;
        return Ok((config.name.clone(), Some(config)));
    }
    if rueca_segment::org::validate_name(arg).is_err() || arg.ends_with(".toml") {
        anyhow::bail!("organization file `{arg}` not found");
    }
    Ok((arg.to_string(), None))
}

#[allow(clippy::too_many_arguments)]
fn cmd_index_org(
    store: &Path,
    org: &str,
    path: Option<PathBuf>,
    repo: Option<String>,
    json: bool,
    pretty: bool,
    threads: Option<usize>,
) -> anyhow::Result<()> {
    if repo.is_some() {
        anyhow::bail!("`--repo` cannot be combined with `--org`; set `id` on the `[[repo]]` in the organization file");
    }
    let (_, config) = org_arg(org)?;
    let Some(config) = config else {
        anyhow::bail!("`rueca index --org` needs the organization file (`rueca-org.toml`), not a name");
    };
    let report = rueca_index::org::index_org(&rueca_index::org::OrgIndexOptions {
        config,
        store: store.to_path_buf(),
        budget: rueca_extract::Budget::default(),
        threads,
        only: path,
    })?;
    if json {
        println!("{}", render(&serde_json::to_value(&report)?, pretty));
        return Ok(());
    }
    for r in &report.indexed {
        let snap = &r.segment.snapshot;
        println!(
            "indexed {} ({} @ {}{}): {} files, {} nodes, {} edges in {} ms",
            r.root,
            snap.repo,
            short(&snap.commit),
            if snap.dirty { ", dirty" } else { "" },
            r.files_total,
            r.segment.node_count,
            r.segment.edge_count,
            r.duration_ms
        );
    }
    for r in &report.refs {
        println!(
            "indexed ref {} of {} ({} @ {}){} in {} ms",
            r.ref_name,
            r.repo,
            r.kind.as_str(),
            short(&r.commit),
            if r.unchanged { ": unchanged" } else { "" },
            r.duration_ms
        );
    }
    for e in &report.ref_errors {
        println!("  ref not indexed: {e}");
    }
    print_org_update(&report.update);
    Ok(())
}

/// `rueca index --org FILE PATH --ref NAME [--remove]`: one ref of one
/// member, then the organization layer.
fn cmd_index_org_ref(store: &Path, org: &str, cmd: RefCmd, out: Output) -> anyhow::Result<()> {
    if cmd.repo.is_some() {
        anyhow::bail!("`--repo` cannot be combined with `--org`; set `id` on the `[[repo]]` in the organization file");
    }
    let (_, config) = org_arg(org)?;
    let Some(config) = config else {
        anyhow::bail!("`rueca index --org` needs the organization file (`rueca-org.toml`), not a name");
    };
    let opts = rueca_index::org::OrgIndexOptions {
        config,
        store: store.to_path_buf(),
        budget: rueca_extract::Budget::default(),
        threads: cmd.threads,
        only: Some(cmd.path.unwrap_or_else(|| PathBuf::from("."))),
    };
    let report = rueca_index::org::index_org_ref(&opts, &cmd.name, cmd.remove)?;
    if out.json {
        let mut v = serde_json::to_value(&report)?;
        if let Some(r) = &report.indexed {
            v["indexed"] = ref_report_json(r)?;
        }
        println!("{}", render(&v, out.pretty));
        return Ok(());
    }
    if let Some(r) = &report.indexed {
        print_ref_report(r);
    }
    if let Some(r) = &report.removed {
        println!("removed ref {} of {} ({})", r.ref_name, r.repo, r.export_dir);
    }
    print_org_update(&report.update);
    Ok(())
}

fn print_org_update(u: &rueca_segment::org::OrgUpdate) {
    println!(
        "organization {}: generation {}{}, {} repositories ({} added, {} replaced, {} removed, {} unchanged) in {} ms",
        u.name,
        u.generation,
        if u.published { "" } else { " (unchanged)" },
        u.repos,
        u.added.len(),
        u.replaced.len(),
        u.removed.len(),
        u.unchanged,
        u.duration_ms
    );
    let ref_work = u.refs_added.len() + u.refs_replaced.len() + u.refs_removed.len();
    if u.refs > 0 || ref_work > 0 {
        println!(
            "  refs:       {} ({} added, {} replaced, {} removed, {} unchanged)",
            u.refs,
            u.refs_added.len(),
            u.refs_replaced.len(),
            u.refs_removed.len(),
            u.refs_unchanged
        );
    }
    for c in &u.costs {
        println!(
            "  {}: copy {} µs ({}), profile {} µs, links {} µs",
            c.entry,
            c.copy_us,
            human_bytes(c.bytes_copied),
            c.profile_us,
            c.link_us
        );
    }
    if !u.missing.is_empty() {
        println!("  not indexed yet (left out): {}", u.missing.join(", "));
    }
    for r in &u.unowned_refs {
        println!("  left out: {r}");
    }
}

/// The default root for `query`/`mcp`: `--root`, else the current directory
/// when it lies inside an indexed root.
fn default_root(store: &Path, root: Option<PathBuf>) -> Option<PathBuf> {
    if root.is_some() {
        return root;
    }
    let cwd = std::env::current_dir().ok()?;
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let entry = Store::open(store).ok()?.entry_for_path(&cwd).ok()??;
    Some(PathBuf::from(entry.root))
}

fn cmd_query(
    store: &Path,
    root: Option<PathBuf>,
    org: Option<&str>,
    tool: &str,
    args: Option<String>,
    pretty: bool,
    text: bool,
) -> ExitCode {
    match run_query(store, root, org, tool, args) {
        Ok(response) if text => {
            print!("{}", response.text);
            if !response.text.ends_with('\n') {
                println!();
            }
            ExitCode::SUCCESS
        }
        Ok(response) => {
            println!("{}", render(&response.json, pretty));
            ExitCode::SUCCESS
        }
        Err(err) => {
            println!("{}", render(&json!({ "error": format!("{err:#}") }), pretty));
            ExitCode::from(2)
        }
    }
}

fn run_query(
    store: &Path,
    root: Option<PathBuf>,
    org: Option<&str>,
    tool: &str,
    args: Option<String>,
) -> anyhow::Result<rueca_query::Response> {
    let raw = match args.as_deref() {
        None => "{}".to_string(),
        Some("-") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
        Some(s) => s.to_string(),
    };
    let args: Value = serde_json::from_str(if raw.trim().is_empty() { "{}" } else { &raw })
        .map_err(|e| anyhow::anyhow!("arguments are not valid JSON: {e}"))?;
    if !rueca_query::TOOL_NAMES.contains(&tool) {
        anyhow::bail!("unknown tool `{tool}` (available: {})", rueca_query::TOOL_NAMES.join(", "));
    }
    query_engine(store, root, org)?.respond(tool, args)
}

/// The engine of `query` and `contracts`: the organization `org` (a file or
/// a built name), else the default root.
fn query_engine(store: &Path, root: Option<PathBuf>, org: Option<&str>) -> anyhow::Result<rueca_query::Engine> {
    match org {
        Some(o) => rueca_query::Engine::for_org(store.to_path_buf(), &org_arg(o)?.0),
        None => rueca_query::Engine::new(store.to_path_buf(), default_root(store, root)),
    }
}

fn render(v: &Value, pretty: bool) -> String {
    if pretty { serde_json::to_string_pretty(v) } else { serde_json::to_string(v) }.unwrap_or_else(|_| v.to_string())
}

fn cmd_mcp(store: &Path, root: Option<PathBuf>, org: Option<&str>, auto_index: bool) -> anyhow::Result<()> {
    if let Some(o) = org {
        let (name, config) = org_arg(o)?;
        return rueca_mcp::serve_stdio(rueca_mcp::ServerOptions {
            store_dir: store.to_path_buf(),
            root: None,
            auto_index,
            org: Some(name),
            org_config: config,
        });
    }
    let explicit = root.is_some();
    let root = default_root(store, root);
    rueca_mcp::serve_stdio(rueca_mcp::ServerOptions {
        store_dir: store.to_path_buf(),
        root,
        // Only an explicit --root is indexed automatically.
        auto_index: auto_index && explicit,
        ..rueca_mcp::ServerOptions::default()
    })
}

/// `rueca links` filters.
struct LinksCmd<'a> {
    /// Only edges leaving this repository.
    repo: Option<&'a str>,
    /// With `repo`: that ref entry's link set.
    ref_name: Option<&'a str>,
}

fn cmd_links(store: &Path, org: &str, cmd: LinksCmd<'_>, out: Output) -> anyhow::Result<()> {
    use rueca_segment::org::{OrgSelection, OrgSlot};
    let (name, _) = org_arg(org)?;
    let idx = rueca_segment::org::OrgIndex::open(store, &name)?;
    let st = Store::open(store)?;
    let g = idx.generation();
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let mut view = OrgSelection::default();
    let mut slots: Vec<OrgSlot> = all.iter().map(|&i| OrgSlot::default_of(i)).collect();
    if let Some(r) = cmd.repo {
        let i = g.repo_index(r).ok_or_else(|| anyhow::anyhow!("repository `{r}` is not in organization `{name}`"))?;
        slots = match cmd.ref_name {
            Some(x) => g.select(i, &[x])?,
            None => vec![OrgSlot::default_of(i)],
        };
        view.set(i, slots.clone());
    }
    let edges = link_rows(&idx, &st, &slots, &view)?;
    if out.json {
        println!("{}", render(&json!({"org": name, "generation": g.generation, "edges": edges}), out.pretty));
        return Ok(());
    }
    for e in &edges {
        let via = match e["via"].as_str() {
            Some("contract") => format!("  [contract {}]", e["key"].as_str().unwrap_or_default()),
            _ => String::new(),
        };
        println!(
            "{} {}  →  {} {}  {} {} {}{via}",
            e["src_repo"].as_str().unwrap_or_default(),
            e["src"].as_str().unwrap_or_default(),
            e["dst_repo"].as_str().unwrap_or_default(),
            e["dst"].as_str().unwrap_or_default(),
            e["kind"].as_str().unwrap_or_default(),
            e["strategy"].as_str().unwrap_or_default(),
            e["confidence"]
        );
    }
    println!("{} cross-repository edges in organization `{name}` (generation {})", edges.len(), g.generation);
    Ok(())
}

/// One JSON row per outgoing link edge of the entries `slots`, seen from
/// `view`.
fn link_rows(
    idx: &rueca_segment::org::OrgIndex,
    st: &Store,
    slots: &[rueca_segment::org::OrgSlot],
    view: &rueca_segment::org::OrgSelection,
) -> anyhow::Result<Vec<Value>> {
    use rueca_segment::org::{LinkDir, OrgSlot};
    let g = idx.generation();
    let all: Vec<usize> = (0..g.repos.len()).collect();
    let mut segs: std::collections::HashMap<OrgSlot, rueca_segment::Segment> = std::collections::HashMap::new();
    for s in g.slots() {
        if let Some(e) = g.entry(s) {
            segs.insert(s, st.open_segment(&e.segment_id)?);
        }
    }
    let rows = LinkRows { g, segs: &segs };
    let mut edges = Vec::new();
    for &at in slots {
        for n in 0..segs[&at].nodes().len() as u32 {
            for e in idx.link_edges(at, n, LinkDir::Out, None, &all, view)? {
                edges.push(rows.row(at, n, &e));
            }
        }
    }
    Ok(edges)
}

/// What a `rueca links` row reads: the generation and every entry's
/// segment.
struct LinkRows<'a> {
    g: &'a OrgGeneration,
    segs: &'a std::collections::HashMap<rueca_segment::org::OrgSlot, rueca_segment::Segment>,
}

impl LinkRows<'_> {
    /// `repo`, or `repo@ref` for a ref entry.
    fn label(&self, s: rueca_segment::org::OrgSlot) -> String {
        match s.ref_idx {
            None => self.g.repos[s.repo].repo.clone(),
            Some(_) => format!("{}@{}", self.g.repos[s.repo].repo, self.g.ref_name(s).unwrap_or_default()),
        }
    }

    /// The JSON row of link edge `e` leaving node `n` of entry `at`.
    fn row(&self, at: rueca_segment::org::OrgSlot, n: u32, e: &rueca_segment::org::LinkRef) -> Value {
        let path_of = |seg: &rueca_segment::Segment, n: u32| seg.node(n).file.map(|f| seg.file(f).path.clone());
        let (seg, dst, ev) = (&self.segs[&at], &self.segs[&e.slot], &self.segs[&e.evidence]);
        let contract = e.meta.strategy.as_str() == "contract";
        let mut row = json!({
            "consumer": self.label(e.evidence),
            "src_repo": self.label(at),
            "src": seg.node(n).qualified_name,
            "src_label": seg.node(n).label.as_str(),
            "src_path": path_of(seg, n),
            "dst_repo": self.label(e.slot),
            "dst": dst.node(e.node).qualified_name,
            "dst_name": dst.node(e.node).name,
            "dst_label": dst.node(e.node).label.as_str(),
            "dst_path": path_of(dst, e.node),
            "kind": e.kind.as_str(),
            "strategy": e.meta.strategy.as_str(),
            "confidence": shortest(e.meta.confidence),
            "path": e.meta.evidence.map(|(f, _)| ev.file(f).path.clone()),
            "line": e.meta.evidence.map(|(_, s)| s.start_line + 1),
            "via": if contract { "contract" } else { "package" },
        });
        if let Some(k) = contract.then(|| contract_key(ev, n, e)).flatten() {
            row["key"] = json!(k);
        }
        row
    }
}

/// `x` as the `f64` with its shortest decimal form (`0.9`, not
/// `0.8999999761581421`).
fn shortest(x: f32) -> f64 {
    format!("{x}").parse().unwrap_or(f64::from(x))
}

/// The contract key of contract link edge `e` leaving node `node`: its
/// consumer row's (the row of `consumer` on that node at the edge's
/// evidence).
fn contract_key(consumer: &rueca_segment::Segment, node: u32, e: &rueca_segment::org::LinkRef) -> Option<String> {
    let (file, span) = e.meta.evidence?;
    let rows = consumer.contracts_of(node);
    rows.iter().find(|r| r.file == file && r.span == span).map(|r| r.key.clone())
}

/// The `status` row of an indexed root (its segment's counts, or the error
/// opening it). `ref` is the snapshot's ref (its branch, `HEAD` when
/// detached, `worktree` outside git); `ref_snapshot` marks a ref exported
/// under the store (`index --ref`).
fn root_row(st: &Store, e: &CatalogEntry, now: u64) -> Value {
    let mut row = json!({
        "root": e.root,
        "repo": e.repo,
        "segment_id": e.segment_id,
        "indexed_at": e.indexed_at,
        "age_secs": now.saturating_sub(e.indexed_at),
        "ref_snapshot": e.is_ref_export(),
    });
    if let Some(r) = &e.ref_name {
        row["ref"] = json!(r);
    }
    match st.open_segment(&e.segment_id) {
        Ok(seg) => {
            let m = seg.meta();
            row["ref"] = json!(ref_label(m.snapshot.branch.as_deref(), &m.snapshot.commit));
            row["commit"] = json!(m.snapshot.commit);
            row["dirty"] = json!(m.snapshot.dirty);
            row["nodes"] = json!(m.node_count);
            row["edges"] = json!(m.edge_count);
            row["files"] = json!(m.file_count);
            row["bytes"] = json!(m.bytes);
        }
        Err(err) => row["error"] = json!(err.to_string()),
    }
    row
}

/// A snapshot's ref as the tools name it (RFC 0005 §4).
fn ref_label(branch: Option<&str>, commit: &str) -> String {
    match branch {
        Some(b) if !b.is_empty() => b.to_string(),
        _ if commit.len() >= 7 && commit.bytes().all(|b| b.is_ascii_hexdigit()) => "HEAD".into(),
        _ => "worktree".into(),
    }
}

/// The `status` row of an organization.
fn org_row(g: &OrgGeneration, now: u64) -> Value {
    json!({
        "name": g.name,
        "generation": g.generation,
        "created_at": g.created_at,
        "age_secs": now.saturating_sub(g.created_at),
        "repos": g.repos.iter().map(|r| json!({
            "repo": r.repo,
            "root": r.default.root,
            "url": r.url,
            "ref": rueca_segment::org::snapshot_ref_name(&r.default.snapshot),
            "commit": r.default.snapshot.commit,
            "segment_id": r.default.segment_id,
            "docs": r.default.docs,
            "index_bytes": r.default.index_bytes,
            "refs": r.refs.iter().map(|x| json!({
                "ref": x.ref_name,
                "commit": x.entry.snapshot.commit,
                "segment_id": x.entry.segment_id,
                "docs": x.entry.docs,
                "index_bytes": x.entry.index_bytes,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn cmd_status(store: &Path, json: bool, pretty: bool) -> anyhow::Result<()> {
    let st = Store::open(store)?;
    let mut entries = st.entries()?;
    entries.sort_by(|a, b| a.root.cmp(&b.root));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let rows: Vec<Value> = entries.iter().map(|e| root_row(&st, e, now)).collect();
    let orgs = rueca_segment::org::list_orgs(store)?;
    let org_rows: Vec<Value> = orgs.iter().map(|g| org_row(g, now)).collect();
    if json {
        println!("{}", render(&json!({ "store": store, "roots": rows, "orgs": org_rows }), pretty));
        return Ok(());
    }
    if rows.is_empty() {
        println!("no indexed roots in {} (run `rueca index <path>`)", store.display());
        return Ok(());
    }
    print_roots(&rows);
    for r in rows.iter().filter(|r| r.get("error").is_some()) {
        eprintln!("rueca: cannot open segment of {}: {}", r["root"].as_str().unwrap_or_default(), r["error"]);
    }
    for g in &orgs {
        print_org(g, now);
    }
    Ok(())
}

/// The table of working-tree roots, each repository's ref snapshots listed
/// under its first root; ref snapshots of repositories without a working
/// tree come last, under their repository.
fn print_roots(rows: &[Value]) {
    let is_ref = |r: &Value| r["ref_snapshot"] == true;
    let repo = |r: &Value| r["repo"].as_str().unwrap_or_default().to_string();
    let own: Vec<&Value> = rows.iter().filter(|r| !is_ref(r)).collect();
    let refs: Vec<&Value> = rows.iter().filter(|r| is_ref(r)).collect();
    let table: Vec<[String; 8]> = own.iter().map(|r| table_cells(r)).collect();
    let header = ["ROOT", "REPO", "REF", "COMMIT", "NODES", "EDGES", "FILES", "AGE"];
    let mut widths = header.map(str::len);
    for row in &table {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .zip(widths)
            .enumerate()
            .map(|(i, (c, w))| if (4..7).contains(&i) { format!("{c:>w$}") } else { format!("{c:<w$}") })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    println!("{}", line(header.to_vec()));
    let mut listed = std::collections::BTreeSet::new();
    for (r, cells) in own.iter().zip(&table) {
        println!("{}", line(cells.iter().map(String::as_str).collect()));
        if listed.insert(repo(r)) {
            refs.iter().filter(|x| repo(x) == repo(r)).for_each(|x| println!("{}", ref_line(x)));
        }
    }
    let orphans: Vec<&Value> = refs.iter().copied().filter(|r| !listed.contains(&repo(r))).collect();
    for r in orphans {
        if listed.insert(repo(r)) {
            println!("{} (no working tree indexed)", repo(r));
        }
        println!("{}", ref_line(r));
    }
}

/// The table cells of a working-tree row.
fn table_cells(r: &Value) -> [String; 8] {
    let commit = r["commit"].as_str().map(|c| format!("{}{}", short(c), if r["dirty"] == true { "*" } else { "" }));
    let num = |k: &str| r[k].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    [
        r["root"].as_str().unwrap_or_default().to_string(),
        r["repo"].as_str().unwrap_or_default().to_string(),
        r["ref"].as_str().unwrap_or("?").to_string(),
        commit.unwrap_or_else(|| "error".into()),
        num("nodes"),
        num("edges"),
        num("files"),
        human_age(r["age_secs"].as_u64().unwrap_or(0)),
    ]
}

/// One ref snapshot under its repository:
/// `  ref feature/x @0123456789ab  812 nodes  3m  (export)`.
fn ref_line(r: &Value) -> String {
    let nodes = r["nodes"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    format!(
        "  ref {} @{}  {nodes} nodes  {}  (export)",
        r["ref"].as_str().unwrap_or("?"),
        r["commit"].as_str().map(short).unwrap_or("error"),
        human_age(r["age_secs"].as_u64().unwrap_or(0))
    )
}

fn print_org(g: &OrgGeneration, now: u64) {
    let entries = || g.repos.iter().flat_map(|r| r.entries());
    let docs: u64 = entries().map(|e| u64::from(e.docs)).sum();
    let bytes: u64 = entries().map(|e| e.index_bytes).sum();
    let refs: usize = g.repos.iter().map(|r| r.refs.len()).sum();
    println!();
    println!(
        "organization {}: generation {}, {} repositories{}, {} symbol documents ({}), {} old",
        g.name,
        g.generation,
        g.repos.len(),
        if refs > 0 { format!(" + {refs} refs") } else { String::new() },
        docs,
        human_bytes(bytes),
        human_age(now.saturating_sub(g.created_at))
    );
    for (i, r) in g.repos.iter().enumerate() {
        let refs: Vec<String> = g.refs_of(i).iter().map(|x| format!("{}@{}", x.ref_name, short(&x.commit))).collect();
        println!("  {}  {}  {}", r.repo, refs.join(" + "), r.default.root);
    }
}

fn short(commit: &str) -> &str {
    if commit.len() > 12 && commit.bytes().all(|b| b.is_ascii_hexdigit()) { &commit[..12] } else { commit }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn human_age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(short("0123456789abcdef0123"), "0123456789ab");
        assert_eq!(short("worktree"), "worktree");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_age(5), "5s");
        assert_eq!(human_age(7200), "2h");
    }
}
