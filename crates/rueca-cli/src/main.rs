//! `rueca` — the Rueca command-line interface. See `docs/tools.md` for the
//! contract.
//!
//! ```text
//! rueca index [PATH] [--store DIR] [--repo ID] [--json]
//! rueca index --org FILE [PATH] [--store DIR] [--json]
//! rueca query <TOOL> '<JSON ARGS>' [--root PATH | --org FILE|NAME] [--store DIR] [--pretty] [--text]
//! rueca mcp [--root PATH | --org FILE|NAME] [--store DIR] [--no-auto-index]
//! rueca status [--store DIR] [--json]
//! ```
//!
//! `--store`, `--root`, `--org`, `-v`, `--json` and `--pretty` are global
//! (accepted before or after the subcommand). `--org` names an organization
//! (RFC 0006): its file (`rueca-org.toml`) or, for `query` and `mcp`, the
//! name of an organization already built in the store. `index --org FILE`
//! indexes every repository of the file (or only the one containing PATH)
//! and updates the organization; a plain `index PATH` of a member also
//! updates the organizations that hold it. JSON output is compact unless `--pretty`.
//! Logs go to stderr (`-v` debug, `-vv` trace, or `RUECA_LOG=<filter>`).
//! `query` prints the tool's JSON result to stdout (with `--text`, the
//! compact text rendering an MCP client gets as text content); on error it
//! prints `{"error": "..."}` and exits with status 2. Other commands report errors on
//! stderr and exit with status 1.
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

use clap::{Parser, Subcommand};
use rueca_segment::Store;
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
    },
    /// Run one tool and print its JSON result. Same semantics as MCP.
    #[command(
        after_help = "Tools: search_symbols, search_code, get_symbol, get_snippet, trace, get_file_outline, list_repositories\n\
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
    /// and the organizations of the store.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose, matches!(cli.command, Command::Mcp { .. }));
    let store = cli.store.clone().unwrap_or_else(Store::default_dir);
    let result = match cli.command {
        Command::Index { path, repo, threads } => match cli.org {
            Some(org) => cmd_index_org(&store, &org, path, repo, cli.json, cli.pretty, threads),
            None => cmd_index(&store, path, repo, cli.json, cli.pretty, threads),
        },
        Command::Query { tool, args, text } => {
            return cmd_query(&store, cli.root, cli.org.as_deref(), &tool, args, cli.pretty, text);
        }
        Command::Mcp { no_auto_index } => cmd_mcp(&store, cli.root, cli.org.as_deref(), !no_auto_index),
        Command::Status => cmd_status(&store, cli.json, cli.pretty),
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
    let s = &report.segment;
    let snap = &s.snapshot;
    let pct = if report.references_total > 0 {
        100.0 * report.references_resolved as f64 / report.references_total as f64
    } else {
        100.0
    };
    println!(
        "indexed {} ({} @ {}{})",
        report.root,
        snap.repo,
        short(&snap.commit),
        if snap.dirty { ", dirty" } else { "" }
    );
    println!(
        "  files:      {} total, {} extracted, {} skipped, {} cache hits",
        report.files_total, report.files_extracted, report.files_skipped, report.facts_cache_hits
    );
    println!("  graph:      {} nodes, {} edges", s.node_count, s.edge_count);
    println!("  references: {}/{} resolved ({pct:.1}%)", report.references_resolved, report.references_total);
    println!("  segment:    {} ({}) in {} ms", s.id, human_bytes(s.bytes), report.duration_ms);
    for u in &orgs {
        print_org_update(u);
    }
    Ok(())
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
    if !u.missing.is_empty() {
        println!("  not indexed yet (left out): {}", u.missing.join(", "));
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
    let engine = match org {
        Some(o) => rueca_query::Engine::for_org(store.to_path_buf(), &org_arg(o)?.0)?,
        None => rueca_query::Engine::new(store.to_path_buf(), default_root(store, root))?,
    };
    engine.respond(tool, args)
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

fn cmd_status(store: &Path, json: bool, pretty: bool) -> anyhow::Result<()> {
    let st = Store::open(store)?;
    let mut entries = st.entries()?;
    entries.sort_by(|a, b| a.root.cmp(&b.root));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut row = json!({
                "root": e.root,
                "repo": e.repo,
                "segment_id": e.segment_id,
                "indexed_at": e.indexed_at,
                "age_secs": now.saturating_sub(e.indexed_at),
            });
            match st.open_segment(&e.segment_id) {
                Ok(seg) => {
                    let m = seg.meta();
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
        })
        .collect();
    let orgs = rueca_segment::org::list_orgs(store)?;
    let org_rows: Vec<Value> = orgs
        .iter()
        .map(|g| {
            json!({
                "name": g.name,
                "generation": g.generation,
                "created_at": g.created_at,
                "age_secs": now.saturating_sub(g.created_at),
                "repos": g.repos.iter().map(|r| json!({
                    "repo": r.repo,
                    "root": r.root,
                    "url": r.url,
                    "commit": r.snapshot.commit,
                    "segment_id": r.segment_id,
                    "docs": r.docs,
                    "index_bytes": r.index_bytes,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    if json {
        println!("{}", render(&json!({ "store": store, "roots": rows, "orgs": org_rows }), pretty));
        return Ok(());
    }
    if rows.is_empty() {
        println!("no indexed roots in {} (run `rueca index <path>`)", store.display());
        return Ok(());
    }
    let table: Vec<[String; 7]> = rows
        .iter()
        .map(|r| {
            let commit =
                r["commit"].as_str().map(|c| format!("{}{}", short(c), if r["dirty"] == true { "*" } else { "" }));
            let num = |k: &str| r[k].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "?".into());
            [
                r["root"].as_str().unwrap_or_default().to_string(),
                r["repo"].as_str().unwrap_or_default().to_string(),
                commit.unwrap_or_else(|| "error".into()),
                num("nodes"),
                num("edges"),
                num("files"),
                human_age(r["age_secs"].as_u64().unwrap_or(0)),
            ]
        })
        .collect();
    let header = ["ROOT", "REPO", "COMMIT", "NODES", "EDGES", "FILES", "AGE"];
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
            .map(|(i, (c, w))| if (3..6).contains(&i) { format!("{c:>w$}") } else { format!("{c:<w$}") })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    println!("{}", line(header.to_vec()));
    for row in &table {
        println!("{}", line(row.iter().map(String::as_str).collect()));
    }
    for r in rows.iter().filter(|r| r.get("error").is_some()) {
        eprintln!("rueca: cannot open segment of {}: {}", r["root"].as_str().unwrap_or_default(), r["error"]);
    }
    for g in &orgs {
        let docs: u64 = g.repos.iter().map(|r| u64::from(r.docs)).sum();
        let bytes: u64 = g.repos.iter().map(|r| r.index_bytes).sum();
        println!();
        println!(
            "organization {}: generation {}, {} repositories, {} symbol documents ({}), {} old",
            g.name,
            g.generation,
            g.repos.len(),
            docs,
            human_bytes(bytes),
            human_age(now.saturating_sub(g.created_at))
        );
        for r in &g.repos {
            println!("  {}  {}  {}", r.repo, short(&r.snapshot.commit), r.root);
        }
    }
    Ok(())
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
