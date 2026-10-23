//! `telar` — the Telar command-line interface. See `docs/tools.md` for the
//! contract.
//!
//! ```text
//! telar index [PATH] [--store DIR] [--repo ID] [--json]
//! telar query <TOOL> '<JSON ARGS>' [--root PATH] [--store DIR] [--pretty]
//! telar mcp [--root PATH] [--store DIR] [--no-auto-index]
//! telar status [--store DIR] [--json]
//! ```
//!
//! `--store`, `--root`, `-v`, `--json` and `--pretty` are global (accepted
//! before or after the subcommand). JSON output is compact unless `--pretty`.
//! Logs go to stderr (`-v` debug, `-vv` trace, or `TELAR_LOG=<filter>`).
//! `query` prints the tool's JSON result to stdout; on error it prints
//! `{"error": "..."}` and exits with status 2. Other commands report errors on
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
use serde_json::{Value, json};
use telar_segment::Store;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "telar", version, about = "Code intelligence for coding agents: index a working tree, query it, serve it over MCP.")]
struct Cli {
    /// Store directory (default: $TELAR_HOME, else ~/.cache/telar).
    #[arg(long, global = true, value_name = "DIR")]
    store: Option<PathBuf>,
    /// Working-tree root used by default by `query` and `mcp`
    /// (default: the indexed root containing the current directory).
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,
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
    /// Index the working tree at PATH (default: current directory).
    Index {
        /// Working tree to index.
        path: Option<PathBuf>,
        /// Repository id override (`github.com/owner/name`, `local/name`).
        #[arg(long, value_name = "ID")]
        repo: Option<String>,
        /// Worker threads (default: available parallelism).
        #[arg(long, value_name = "N")]
        threads: Option<usize>,
    },
    /// Run one tool and print its JSON result. Same semantics as MCP.
    #[command(after_help = "Tools: search_symbols, search_code, get_symbol, get_snippet, trace, get_file_outline\n\
Example: telar query trace '{\"qualified_name\": \"InvoiceService.issue\", \"direction\": \"callers\"}'")]
    Query {
        /// Tool name.
        tool: String,
        /// Tool arguments as a JSON object (`-` reads stdin). Default `{}`.
        args: Option<String>,
    },
    /// Serve MCP on stdio. Indexes --root first if it has no segment.
    Mcp {
        /// Do not index --root automatically.
        #[arg(long)]
        no_auto_index: bool,
    },
    /// List indexed roots with repo, commit, node/edge/file counts and age.
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose, matches!(cli.command, Command::Mcp { .. }));
    let store = cli.store.clone().unwrap_or_else(Store::default_dir);
    let result = match cli.command {
        Command::Index { path, repo, threads } => cmd_index(&store, path, repo, cli.json, cli.pretty, threads),
        Command::Query { tool, args } => return cmd_query(&store, cli.root, &tool, args, cli.pretty),
        Command::Mcp { no_auto_index } => cmd_mcp(&store, cli.root, !no_auto_index),
        Command::Status => cmd_status(&store, cli.json, cli.pretty),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("telar: {err:#}");
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
    let filter = EnvFilter::try_from_env("TELAR_LOG").unwrap_or_else(|_| EnvFilter::new(default));
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
    let opts = telar_index::IndexOptions {
        root,
        store: store.to_path_buf(),
        repo,
        budget: telar_extract::Budget::default(),
        threads,
    };
    let report = telar_index::index_worktree(&opts)?;
    if json {
        println!("{}", render(&serde_json::to_value(&report)?, pretty));
        return Ok(());
    }
    let s = &report.segment;
    let snap = &s.snapshot;
    let pct = if report.references_total > 0 {
        100.0 * report.references_resolved as f64 / report.references_total as f64
    } else {
        100.0
    };
    println!("indexed {} ({} @ {}{})", report.root, snap.repo, short(&snap.commit), if snap.dirty { ", dirty" } else { "" });
    println!(
        "  files:      {} total, {} extracted, {} skipped, {} cache hits",
        report.files_total, report.files_extracted, report.files_skipped, report.facts_cache_hits
    );
    println!("  graph:      {} nodes, {} edges", s.node_count, s.edge_count);
    println!(
        "  references: {}/{} resolved ({pct:.1}%)",
        report.references_resolved, report.references_total
    );
    println!("  segment:    {} ({}) in {} ms", s.id, human_bytes(s.bytes), report.duration_ms);
    Ok(())
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

fn cmd_query(store: &Path, root: Option<PathBuf>, tool: &str, args: Option<String>, pretty: bool) -> ExitCode {
    match run_query(store, root, tool, args) {
        Ok(value) => {
            println!("{}", render(&value, pretty));
            ExitCode::SUCCESS
        }
        Err(err) => {
            println!("{}", render(&json!({ "error": format!("{err:#}") }), pretty));
            ExitCode::from(2)
        }
    }
}

fn run_query(store: &Path, root: Option<PathBuf>, tool: &str, args: Option<String>) -> anyhow::Result<Value> {
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
    if !telar_query::TOOL_NAMES.contains(&tool) {
        anyhow::bail!("unknown tool `{tool}` (available: {})", telar_query::TOOL_NAMES.join(", "));
    }
    let engine = telar_query::Engine::new(store.to_path_buf(), default_root(store, root))?;
    engine.call(tool, args)
}

fn render(v: &Value, pretty: bool) -> String {
    if pretty { serde_json::to_string_pretty(v) } else { serde_json::to_string(v) }.unwrap_or_else(|_| v.to_string())
}

fn cmd_mcp(store: &Path, root: Option<PathBuf>, auto_index: bool) -> anyhow::Result<()> {
    let explicit = root.is_some();
    let root = default_root(store, root);
    telar_mcp::serve_stdio(telar_mcp::ServerOptions {
        store_dir: store.to_path_buf(),
        root,
        // Only an explicit --root is indexed automatically.
        auto_index: auto_index && explicit,
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
    if json {
        println!("{}", render(&json!({ "store": store, "roots": rows }), pretty));
        return Ok(());
    }
    if rows.is_empty() {
        println!("no indexed roots in {} (run `telar index <path>`)", store.display());
        return Ok(());
    }
    let table: Vec<[String; 7]> = rows
        .iter()
        .map(|r| {
            let commit = r["commit"].as_str().map(|c| format!("{}{}", short(c), if r["dirty"] == true { "*" } else { "" }));
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
        eprintln!("telar: cannot open segment of {}: {}", r["root"].as_str().unwrap_or_default(), r["error"]);
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
