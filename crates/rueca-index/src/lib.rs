//! Local indexing pipeline (RFC 0001 §7): walk a working tree, compute blob
//! SHAs, extract facts in parallel (cached by blob + extractor version),
//! resolve, and write a segment into the local store.
//!
//! Phases of [`index_worktree`]:
//!
//! 1. **Walk** the canonicalized root with the `ignore` crate: `.gitignore`,
//!    `.ignore`, `.git/info/exclude` and global git excludes are honored (also
//!    outside git repositories), plus a fixed list of dependency / build /
//!    cache directories, minified bundles, source maps and lockfiles (see
//!    [`EXCLUDED_DIRS`], [`is_excluded_file`]). Hidden files are *not*
//!    skipped (`.github/workflows/*.yml` matters); symlinks are not followed
//!    and not listed. Every remaining regular file is listed in the file table.
//! 2. **Read and extract** in parallel (`rayon`, `IndexOptions::threads`):
//!    each file is read once and its git blob SHA computed. Files larger than
//!    the budget (`skipped:size`) or binary (NUL in the first 8 KiB,
//!    `skipped:binary`) are listed but not extracted. Files with a language
//!    pack are extracted under `catch_unwind` (`skipped:panic`); extractor
//!    errors become `skipped:<reason>` (`size`, `timeout`, `parser`, `query`,
//!    `unsupported`), parse errors `partial`. Files without a pack are
//!    `skipped:unsupported` (still listed and content-searchable).
//! 3. **Facts cache**: `<store>/facts/<sha[0..2]>/<sha>-<lang>-v<version>.bin`
//!    holds `zstd(postcard(FileFacts))`, checked before extracting and written
//!    atomically (temp file + rename) after a successful extraction.
//! 4. **Snapshot identity** from git, read with `gix` (see the `git` module):
//!    repository id from `remote.origin.url` (normalized by
//!    [`normalize_remote`]) else `local/<dirname>`; commit = `HEAD` id, else
//!    `"worktree"`; `dirty` = indexed content differs from the `HEAD` tree
//!    (computed from the blob SHAs of step 2, no `git` process).
//! 5. **Resolve** with `rueca_resolve::resolve`, **write** the segment with
//!    `rueca_segment::write_segment` (contents served from memory, or re-read
//!    from disk and verified against the blob SHA when more than 512 MiB of
//!    content was read), and **register** it in the store catalog under the
//!    canonical root.
//!
//! Logging: one `info` summary per run and `debug` timings per phase
//! (`tracing`). [`IndexReport`] carries the same numbers.

use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rayon::prelude::*;
use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles, Registry};
use rueca_model::{FileEntry, FileFacts, Language, SnapshotInfo, git_blob_sha};
use rueca_resolve::FileInput;
use rueca_segment::{ContentSource, SegmentMeta, Store};
use tracing::{debug, info, warn};

mod git;

#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Working-tree root to index.
    pub root: PathBuf,
    /// Store directory (see `rueca_segment::Store`).
    pub store: PathBuf,
    /// Repository id override; default `local/<dirname>` or derived from the
    /// git `origin` remote (`github.com/owner/name`).
    pub repo: Option<String>,
    pub budget: Budget,
    /// Worker threads; `None` = available parallelism.
    pub threads: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexReport {
    pub segment: SegmentMeta,
    pub root: String,
    pub files_total: u64,
    pub files_extracted: u64,
    pub files_skipped: u64,
    pub facts_cache_hits: u64,
    pub references_total: u64,
    pub references_resolved: u64,
    pub duration_ms: u64,
    /// Extracted files with parse errors (included in `files_extracted`).
    pub files_partial: u64,
    /// Files without a language pack (docs, configs, other languages).
    pub files_unsupported: u64,
    /// Resolved references per strategy name.
    pub resolved_by_strategy: std::collections::BTreeMap<String, u64>,
    /// Unresolved references to builtins.
    pub references_builtin: u64,
    /// Unresolved references through names imported from non-project modules.
    pub references_external: u64,
    /// Wall-clock time per phase, in milliseconds.
    pub timings: PhaseTimings,
}

/// Wall-clock milliseconds per indexing phase.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PhaseTimings {
    pub walk_ms: u64,
    pub extract_ms: u64,
    pub resolve_ms: u64,
    pub write_ms: u64,
}

/// Language packs shipped with this build (Python, TypeScript/TSX/JavaScript,
/// Markdown).
pub fn default_registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(rueca_lang_python::PythonPack));
    r.register(Box::new(rueca_lang_typescript::TypeScriptPack));
    r.register(Box::new(rueca_lang_markdown::MarkdownPack));
    r
}

/// Directory names never descended into.
pub const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "bower_components",
    "vendor",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    "coverage",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    "site-packages",
];

/// Lockfile names excluded from the file table.
pub const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "bun.lock",
    "poetry.lock",
    "Pipfile.lock",
    "pdm.lock",
    "uv.lock",
    "Cargo.lock",
    "composer.lock",
    "Gemfile.lock",
    "go.sum",
];

/// True for directory names excluded by the hard-coded list.
pub fn is_excluded_dir(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name)
}

/// True for file names excluded by the hard-coded list (minified bundles,
/// source maps, lockfiles).
pub fn is_excluded_file(name: &str) -> bool {
    // `.git` is a file in linked worktrees and submodules.
    name == ".git" || name.ends_with(".min.js") || name.ends_with(".map") || LOCKFILES.contains(&name)
}

/// Bytes inspected for NUL when detecting binary files.
const BINARY_SNIFF: usize = 8 * 1024;
/// Total file bytes kept in memory for the segment writer; beyond this the
/// writer's content source re-reads (and verifies) files from disk.
const CONTENT_MEMORY_CAP: u64 = 512 * 1024 * 1024;
/// Largest config file loaded into `ProjectFiles::configs`.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Indexes the working tree at `opts.root` (including uncommitted changes and
/// untracked, non-ignored files) and registers the new segment in the store.
pub fn index_worktree(opts: &IndexOptions) -> anyhow::Result<IndexReport> {
    let started = Instant::now();
    let root = fs::canonicalize(&opts.root).with_context(|| format!("cannot access {}", opts.root.display()))?;
    anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
    let root_str = root.to_string_lossy().into_owned();
    let mut timings = PhaseTimings::default();

    // 1. Walk.
    let t = Instant::now();
    let paths = walk(&root)?;
    timings.walk_ms = t.elapsed().as_millis() as u64;
    debug!(files = paths.len(), ms = timings.walk_ms, "walk");

    let store = Store::open(&opts.store).with_context(|| format!("cannot open store {}", opts.store.display()))?;
    let registry = default_registry();

    // 2–3. Read, hash, extract (cached).
    let t = Instant::now();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.unwrap_or(0))
        .thread_name(|i| format!("rueca-index-{i}"))
        .build()?;
    let facts_dir = opts.store.join("facts");
    let kept_bytes = AtomicU64::new(0);
    let ctx = ExtractCtx {
        root: &root,
        registry: &registry,
        budget: &opts.budget,
        facts_dir: &facts_dir,
        kept_bytes: &kept_bytes,
    };
    let loaded: Vec<anyhow::Result<Loaded>> = pool.install(|| paths.par_iter().map(|p| ctx.load(p)).collect());
    let mut files = Vec::with_capacity(loaded.len());
    let mut contents: HashMap<String, Vec<u8>> = HashMap::new();
    let mut configs = std::collections::BTreeMap::new();
    let (mut extracted, mut skipped, mut partial, mut unsupported, mut hits) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for l in loaded {
        let l = match l {
            Ok(l) => l,
            Err(e) => {
                // Vanished or unreadable since the walk: leave it out.
                warn!("skipping unreadable file: {e:#}");
                continue;
            }
        };
        if l.cache_hit {
            hits += 1;
        }
        match l.input.status.as_str() {
            "ok" => extracted += 1,
            "partial" => {
                extracted += 1;
                partial += 1;
            }
            "skipped:unsupported" => unsupported += 1,
            _ => skipped += 1,
        }
        if let Some(text) = l.config {
            configs.insert(l.input.path.clone(), text);
        }
        if let Some(bytes) = l.bytes {
            contents.insert(l.input.path.clone(), bytes);
        }
        files.push(l.input);
    }
    // Only files that were read are part of the project (vanished ones are out).
    let project = ProjectFiles { paths: files.iter().map(|f| f.path.clone()).collect(), configs };
    timings.extract_ms = t.elapsed().as_millis() as u64;
    debug!(extracted, skipped, partial, unsupported, cache_hits = hits, ms = timings.extract_ms, "extract");

    // 4. Resolve.
    let t = Instant::now();
    let snapshot = snapshot_info(&root, opts.repo.as_deref(), &files);
    let files_total = files.len() as u64;
    let out = catch_unwind(AssertUnwindSafe(|| rueca_resolve::resolve(snapshot, files, &project, &registry)))
        .map_err(|_| anyhow::anyhow!("resolver panicked (a language pack's module hooks failed)"))?;
    if let Err(e) = out.graph.validate() {
        anyhow::bail!("resolver produced an invalid graph: {e}");
    }
    timings.resolve_ms = t.elapsed().as_millis() as u64;
    debug!(nodes = out.graph.nodes.len(), edges = out.graph.edges.len(), ms = timings.resolve_ms, "resolve");

    // 5. Write and register.
    let t = Instant::now();
    let source = WorktreeContent { root: &root, contents: std::sync::Mutex::new(contents) };
    let meta = rueca_segment::write_segment(&out.graph, &source, &opts.store).context("writing segment")?;
    store.register(&root_str, &meta).context("registering segment")?;
    timings.write_ms = t.elapsed().as_millis() as u64;
    debug!(segment = %meta.id, bytes = meta.bytes, ms = timings.write_ms, "write");

    let stats = out.stats;
    let report = IndexReport {
        segment: meta,
        root: root_str,
        files_total,
        files_extracted: extracted,
        files_skipped: skipped,
        facts_cache_hits: hits,
        references_total: stats.references_total,
        references_resolved: stats.references_resolved,
        duration_ms: started.elapsed().as_millis() as u64,
        files_partial: partial,
        files_unsupported: unsupported,
        resolved_by_strategy: stats.resolved_by_strategy,
        references_builtin: stats.references_builtin,
        references_external: stats.references_external,
        timings,
    };
    info!(
        root = %report.root,
        repo = %report.segment.snapshot.repo,
        files = report.files_total,
        extracted = report.files_extracted,
        skipped = report.files_skipped,
        cache_hits = report.facts_cache_hits,
        nodes = report.segment.node_count,
        edges = report.segment.edge_count,
        refs = report.references_total,
        resolved = report.references_resolved,
        ms = report.duration_ms,
        "indexed"
    );
    Ok(report)
}

// ---------------------------------------------------------------------------
// Walk
// ---------------------------------------------------------------------------

/// Repository-relative paths (`/` separators) of every indexable regular file
/// under `root`, sorted.
fn walk(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        // Hidden files are listed (`.github/workflows`, `.eslintrc.js`): only
        // ignore rules and the hard excludes decide.
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_some_and(|t| t.is_dir()) {
                e.depth() == 0 || !is_excluded_dir(&name)
            } else {
                !is_excluded_file(&name)
            }
        });
    let mut out = Vec::new();
    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("walk: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let mut parts = Vec::new();
        let mut ok = true;
        for c in rel.components() {
            match c.as_os_str().to_str() {
                Some(s) => parts.push(s),
                None => ok = false,
            }
        }
        if !ok || parts.is_empty() {
            warn!("walk: skipping non-UTF-8 path {}", entry.path().display());
            continue;
        }
        out.push(parts.join("/"));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Read + extract
// ---------------------------------------------------------------------------

struct ExtractCtx<'a> {
    root: &'a Path,
    registry: &'a Registry,
    budget: &'a Budget,
    facts_dir: &'a Path,
    kept_bytes: &'a AtomicU64,
}

struct Loaded {
    input: FileInput,
    /// Content kept for the segment writer (`None` = re-read from disk).
    bytes: Option<Vec<u8>>,
    /// Text of a module-resolution config file (`ProjectFiles::is_config_file`).
    config: Option<String>,
    cache_hit: bool,
}

impl ExtractCtx<'_> {
    fn load(&self, rel: &str) -> anyhow::Result<Loaded> {
        let abs = self.root.join(rel);
        let bytes = fs::read(&abs).with_context(|| format!("reading {}", abs.display()))?;
        let blob_sha = git_blob_sha(&bytes);
        let size = bytes.len() as u64;
        let pack = self.registry.for_path(rel);
        let language = Language::from_path(rel);
        let mut cache_hit = false;
        let (facts, status) = if bytes.len() > self.budget.max_bytes {
            (None, "skipped:size".to_string())
        } else if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
            (None, "skipped:binary".to_string())
        } else if let Some((lang, pack)) = pack {
            let cache = self.cache_path(&blob_sha, lang, pack.extractor_version());
            match read_cached(&cache) {
                Some(f) => {
                    cache_hit = true;
                    let status = status_of(&f);
                    (Some(f), status)
                }
                None => match self.extract(pack, rel, lang, &bytes) {
                    Ok(f) => {
                        if let Err(e) = write_cached(&cache, &f) {
                            debug!("facts cache write failed for {rel}: {e:#}");
                        }
                        let status = status_of(&f);
                        (Some(f), status)
                    }
                    Err(reason) => (None, format!("skipped:{reason}")),
                },
            }
        } else {
            (None, "skipped:unsupported".to_string())
        };
        let keep = bytes.len() <= self.budget.max_bytes
            && self.kept_bytes.fetch_add(size, Ordering::Relaxed) + size <= CONTENT_MEMORY_CAP;
        let config = (ProjectFiles::is_config_file(rel) && size <= MAX_CONFIG_BYTES)
            .then(|| String::from_utf8(bytes.clone()).ok())
            .flatten();
        let input = FileInput { path: rel.to_string(), blob_sha, language, size, facts, status };
        Ok(Loaded { input, bytes: keep.then_some(bytes), config, cache_hit })
    }

    fn extract(&self, pack: &dyn LanguagePack, rel: &str, lang: Language, bytes: &[u8]) -> Result<FileFacts, String> {
        match catch_unwind(AssertUnwindSafe(|| pack.extract(rel, lang, bytes, self.budget))) {
            Ok(Ok(f)) => Ok(f),
            Ok(Err(e)) => {
                debug!("extract {rel}: {e}");
                Err(match e {
                    ExtractError::TooLarge { .. } => "size".into(),
                    ExtractError::Timeout => "timeout".into(),
                    ExtractError::Parser(_) => "parser".into(),
                    ExtractError::Unsupported(_) => "unsupported".into(),
                    ExtractError::Query(_) => "query".into(),
                })
            }
            Err(_) => {
                warn!("extractor panicked on {rel}");
                Err("panic".into())
            }
        }
    }

    fn cache_path(&self, sha: &str, lang: Language, version: u32) -> PathBuf {
        let prefix = sha.get(..2).unwrap_or("00");
        self.facts_dir.join(prefix).join(format!("{sha}-{}-v{version}.bin", lang.as_str()))
    }
}

fn status_of(f: &FileFacts) -> String {
    if f.parse_errors > 0 { "partial".into() } else { "ok".into() }
}

fn read_cached(path: &Path) -> Option<FileFacts> {
    let compressed = fs::read(path).ok()?;
    let raw = zstd::decode_all(compressed.as_slice()).ok()?;
    postcard::from_bytes(&raw).ok()
}

fn write_cached(path: &Path, facts: &FileFacts) -> anyhow::Result<()> {
    let dir = path.parent().context("cache path has no parent")?;
    fs::create_dir_all(dir)?;
    let raw = postcard::to_stdvec(facts)?;
    let compressed = zstd::encode_all(raw.as_slice(), 3)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, &compressed)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Content source
// ---------------------------------------------------------------------------

/// Serves file bytes read during extraction; each buffer is handed over (not
/// copied) on first request. Files not kept in memory are re-read and
/// verified against their blob SHA.
struct WorktreeContent<'a> {
    root: &'a Path,
    contents: std::sync::Mutex<HashMap<String, Vec<u8>>>,
}

impl ContentSource for WorktreeContent<'_> {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        let kept = self.contents.lock().unwrap_or_else(|p| p.into_inner()).remove(&file.path);
        if let Some(c) = kept {
            return Ok(c);
        }
        let bytes = fs::read(self.root.join(&file.path))?;
        if git_blob_sha(&bytes) != file.blob_sha {
            return Err(std::io::Error::other(format!("{} changed while indexing", file.path)));
        }
        Ok(bytes)
    }
}

// ---------------------------------------------------------------------------
// Snapshot info
// ---------------------------------------------------------------------------

/// Snapshot identity from git (via `gix`, see [`git`]): repo id from
/// `remote.origin.url` (else `local/<dirname>`), commit = `HEAD` (else
/// `"worktree"`), dirty = indexed content differs from `HEAD`.
fn snapshot_info(root: &Path, repo_override: Option<&str>, files: &[FileInput]) -> SnapshotInfo {
    let info = git::read(root);
    let repo = repo_override
        .map(str::to_string)
        .or_else(|| info.as_ref().and_then(|i| i.repo_id.clone()))
        .unwrap_or_else(|| {
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
            format!("local/{name}")
        });
    let commit = info.as_ref().and_then(|i| i.commit.clone()).unwrap_or_else(|| "worktree".into());
    let dirty = match &info {
        Some(i) => git::is_dirty(i, root, files.iter().map(|f| (f.path.as_str(), f.blob_sha.as_str()))),
        None => true,
    };
    SnapshotInfo {
        repo,
        commit,
        dirty,
        root: Some(root.to_string_lossy().into_owned()),
        indexed_at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        rueca_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Normalizes a git remote URL to `host/owner/name`:
/// `git@github.com:o/r.git`, `https://user@github.com/o/r(.git)`,
/// `ssh://git@host:22/o/r` → `github.com/o/r` / `host/o/r`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = host.split(':').next()?;
        (host, path)
    } else if let Some((user_host, path)) = url.split_once(':') {
        // scp-like `git@host:owner/name`.
        if user_host.contains('/') {
            return None;
        }
        (user_host.rsplit('@').next()?, path)
    } else {
        return None;
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_normalization() {
        assert_eq!(normalize_remote("git@github.com:o/r.git").as_deref(), Some("github.com/o/r"));
        assert_eq!(normalize_remote("https://github.com/o/r").as_deref(), Some("github.com/o/r"));
        assert_eq!(normalize_remote("https://github.com/o/r.git").as_deref(), Some("github.com/o/r"));
        assert_eq!(normalize_remote("https://user:tok@GitHub.com/o/r.git/").as_deref(), Some("github.com/o/r"));
        assert_eq!(
            normalize_remote("ssh://git@gitlab.example.com:2222/g/sub/r.git").as_deref(),
            Some("gitlab.example.com/g/sub/r")
        );
        assert_eq!(normalize_remote("/local/path/repo"), None);
        assert_eq!(normalize_remote(""), None);
    }

    #[test]
    fn exclusions() {
        assert!(is_excluded_dir("node_modules"));
        assert!(is_excluded_dir("__pycache__"));
        assert!(!is_excluded_dir("src"));
        assert!(is_excluded_file("app.min.js"));
        assert!(is_excluded_file("app.js.map"));
        assert!(is_excluded_file("package-lock.json"));
        assert!(!is_excluded_file("package.json"));
    }

    #[test]
    fn walk_respects_ignores_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |p: &str, c: &[u8]| {
            let path = root.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        };
        write(".gitignore", b"generated/\n*.log\n");
        write("src/a.py", b"x = 1\n");
        write("generated/b.py", b"y = 2\n");
        write("debug.log", b"log\n");
        write("node_modules/m/index.js", b"//\n");
        write("web/app.min.js", b"//\n");
        write("yarn.lock", b"\n");
        write(".github/ci.yml", b"on: push\n");
        write(".git/config", b"\n");
        write("sub/.git", b"gitdir: ../x\n");
        write("venv/lib/x.py", b"\n");
        write("pkg/__pycache__/a.pyc", b"\x00");
        write("README.md", b"# hi\n");
        let paths = walk(root).unwrap();
        let expected = [".github/ci.yml", ".gitignore", "README.md", "src/a.py"];
        assert_eq!(paths, expected.map(String::from).to_vec());
    }

    #[test]
    fn facts_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab").join("x.bin");
        let facts = FileFacts { parse_errors: 2, line_count: 7, ..Default::default() };
        write_cached(&path, &facts).unwrap();
        assert_eq!(read_cached(&path), Some(facts));
        assert_eq!(read_cached(&dir.path().join("missing.bin")), None);
    }
}
