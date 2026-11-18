//! Bare git mirrors for the service's indexer (RFC 0008 §7.6): one bare
//! repository per code-host repository, fetched one commit at a time at
//! depth 1 with large blobs filtered out, read with `gix`.
//!
//! [`Mirror::fetch_commit`] runs, in the mirror (`git -C <mirror>`):
//!
//! ```sh
//! GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= GIT_CONFIG_NOSYSTEM=1 GIT_NO_LAZY_FETCH=1 \
//! git -c protocol.version=2 -c protocol.allow=never \
//!     -c protocol.https.allow=always -c protocol.ssh.allow=always \
//!     fetch --quiet --depth=1 --no-tags --atomic --no-auto-maintenance \
//!     --no-write-fetch-head --filter=blob:limit=<max_bytes + 1> \
//!     <url> +<commit>:refs/rueca/commit/<commit>
//! ```
//!
//! followed by the caller's extra `-c` options and environment
//! ([`Mirror::with_git`]: `protocol.file.allow=always` for test hosts,
//! credentials as `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_<n>` /
//! `GIT_CONFIG_VALUE_<n>` — never on the command line, in the URL or in the
//! mirror's config). A commit already in the mirror is not fetched again.
//!
//! **Fallback.** Wanting a commit by id needs the host to allow it
//! (`uploadpack.allowReachableSHA1InWant` or the protocol v2 default). When
//! the commit fetch fails, each fallback ref (`refs/heads/main`,
//! `refs/pull/7/head`, …) is fetched the same way into
//! `refs/rueca/fallback` and its tip compared with the wanted commit: equal,
//! the commit is recorded as `refs/rueca/commit/<commit>`; different, the
//! ref moved since it was listed ([`RefMoved`]: the caller retries later,
//! with the new commit).
//!
//! **Partial clone.** The filter makes the mirror a partial clone (git
//! records the URL as a promisor remote): blobs of `max_bytes + 1` bytes or
//! more are absent. Nothing reads them lazily: `gix` never fetches, and
//! every `git` run sets `GIT_NO_LAZY_FETCH=1` (git ≥ 2.45). The export
//! ([`crate::refs::export_with`] with a gap size) lists them as coverage
//! gaps (`skipped:size`).
//!
//! **Expiry.** A mirror is never repacked (`gc.auto = 0`,
//! `maintenance.auto = false`, `--no-auto-maintenance`: repacking a partial
//! clone keeps unreachable objects in promisor packs); it is deleted and
//! re-created once older than the maximum age ([`Mirror::open_with`]), and
//! [`expire_mirrors`] deletes mirrors not used for that long. The marker
//! `rueca-mirror.json` (URL, creation and last-use times) identifies a
//! mirror; a directory without it is never deleted by the sweep.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// The marker file inside a mirror.
pub const MIRROR_MARKER: &str = "rueca-mirror.json";

/// Protocol options of every fetch (RFC 0008 §7.6).
const PROTOCOL: [&str; 4] =
    ["protocol.version=2", "protocol.allow=never", "protocol.https.allow=always", "protocol.ssh.allow=always"];

/// Where a fallback ref is fetched to before its tip is compared.
const FALLBACK_REF: &str = "refs/rueca/fallback";

/// What the marker records.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MirrorInfo {
    url: String,
    /// Unix seconds.
    created_at: u64,
    /// Unix seconds of the last [`Mirror::open_with`].
    used_at: u64,
}

impl MirrorInfo {
    fn read(dir: &Path) -> Option<MirrorInfo> {
        serde_json::from_slice(&fs::read(dir.join(MIRROR_MARKER)).ok()?).ok()
    }

    fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = dir.join(MIRROR_MARKER);
        fs::write(&path, serde_json::to_vec(self)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// A bare mirror of one repository.
#[derive(Debug, Clone)]
pub struct Mirror {
    dir: PathBuf,
    url: String,
    config: Vec<String>,
    env: Vec<(String, String)>,
}

/// How [`Mirror::fetch_commit`] got the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The full commit id.
    pub commit: String,
    /// `commit` (fetched by id), `present` (already in the mirror) or the
    /// fallback ref whose tip it was.
    pub via: String,
}

/// The commit fetch was refused and the fallback ref's tip is another
/// commit: the ref moved since it was listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefMoved {
    pub ref_name: String,
    pub tip: String,
    pub wanted: String,
}

impl fmt::Display for RefMoved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the host refused commit {} by id and {} is now at {}: the ref moved, retry later",
            self.wanted, self.ref_name, self.tip
        )
    }
}

impl std::error::Error for RefMoved {}

impl Mirror {
    /// Opens the mirror of `url` in `dir`, creating a bare repository on
    /// first use. Never expires it (see [`Mirror::open_with`]).
    pub fn open(dir: &Path, url: &str) -> anyhow::Result<Mirror> {
        Mirror::open_with(dir, url, None, unix_now())
    }

    /// [`Mirror::open`]; a mirror older than `max_age` at `now` (unix
    /// seconds), of another URL, or without a marker is deleted and
    /// created again. Records the use in the marker.
    pub fn open_with(dir: &Path, url: &str, max_age: Option<Duration>, now: u64) -> anyhow::Result<Mirror> {
        anyhow::ensure!(!url.trim().is_empty() && !url.starts_with('-'), "`{url}` is not a repository URL");
        let info = MirrorInfo::read(dir);
        let expired = |i: &MirrorInfo| max_age.is_some_and(|a| now.saturating_sub(i.created_at) > a.as_secs());
        let keep = info.as_ref().filter(|i| i.url == url && !expired(i) && dir.join("HEAD").is_file());
        let created_at = match keep {
            Some(i) => i.created_at,
            None => {
                if dir.exists() {
                    tracing::info!(mirror = %dir.display(), "re-creating the git mirror (expired, moved or incomplete)");
                    fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
                }
                init_bare(dir)?;
                now
            }
        };
        MirrorInfo { url: url.to_string(), created_at, used_at: now }.write(dir)?;
        Ok(Mirror { dir: dir.to_path_buf(), url: url.to_string(), config: Vec::new(), env: Vec::new() })
    }

    /// Adds `-c` options (after the fixed protocol options, so they win)
    /// and environment variables (credentials) to every `git` run.
    pub fn with_git(mut self, config: Vec<String>, env: Vec<(String, String)>) -> Mirror {
        self.config = config;
        self.env = env;
        self
    }

    /// The bare repository's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the mirror holds commit `sha` (as fetched by
    /// [`Mirror::fetch_commit`]).
    pub fn has_commit(&self, sha: &str) -> bool {
        self.ref_commit(&commit_ref(sha)).ok().flatten().is_some_and(|c| c == sha)
    }

    /// Fetches commit `sha` (40 hex digits) at depth 1 without blobs of
    /// more than `max_bytes` bytes (see the module docs); when the host
    /// refuses the commit, through `fallback` refs (full names) whose tip
    /// must be `sha` ([`RefMoved`] otherwise).
    pub fn fetch_commit(&self, sha: &str, max_bytes: u64, fallback: &[String]) -> anyhow::Result<Fetched> {
        let sha = sha.to_ascii_lowercase();
        anyhow::ensure!(sha.len() == 40 && crate::refs::is_hex(&sha), "`{sha}` is not a full commit id");
        if self.has_commit(&sha) {
            return Ok(Fetched { commit: sha, via: "present".into() });
        }
        let filter = format!("--filter=blob:limit={}", max_bytes.saturating_add(1));
        let first = match self.fetch(&filter, &format!("+{sha}:{}", commit_ref(&sha))) {
            Ok(()) => return Ok(Fetched { commit: sha, via: "commit".into() }),
            Err(e) => e,
        };
        let mut errors = vec![format!("{first:#}")];
        for name in fallback {
            match self.fetch_fallback(&filter, name, &sha) {
                Ok(f) => return Ok(f),
                Err(e) if e.downcast_ref::<RefMoved>().is_some() => return Err(e),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        anyhow::bail!("cannot fetch commit {sha} from {}: {}", self.url, errors.join("; "))
    }

    /// One fallback ref: fetched to [`FALLBACK_REF`], its tip compared with
    /// `sha`.
    fn fetch_fallback(&self, filter: &str, name: &str, sha: &str) -> anyhow::Result<Fetched> {
        anyhow::ensure!(name.starts_with("refs/"), "fallback `{name}` is not a full ref name");
        crate::refs::validate_ref_name(name)?;
        self.fetch(filter, &format!("+{name}:{FALLBACK_REF}"))?;
        let tip = self.ref_commit(FALLBACK_REF)?.with_context(|| format!("{FALLBACK_REF} missing after a fetch"))?;
        if tip != sha {
            return Err(RefMoved { ref_name: name.to_string(), tip, wanted: sha.to_string() }.into());
        }
        self.git(&["update-ref", &commit_ref(sha), sha])?;
        tracing::info!(mirror = %self.dir.display(), r#ref = name, commit = sha, "commit fetched through its ref");
        Ok(Fetched { commit: sha.to_string(), via: name.to_string() })
    }

    /// `git fetch … <filter> <url> <refspec>` (module docs).
    fn fetch(&self, filter: &str, refspec: &str) -> anyhow::Result<()> {
        let fetch = [
            "fetch",
            "--quiet",
            "--depth=1",
            "--no-tags",
            "--atomic",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            filter,
            self.url.as_str(),
            refspec,
        ];
        self.git(&fetch).map(|_| ())
    }

    /// `git -c <protocol> -c <extra> <args>` in the mirror, with the
    /// no-prompt, no-system-config and no-lazy-fetch environment.
    fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut all: Vec<&str> = Vec::with_capacity(2 * (PROTOCOL.len() + self.config.len()) + args.len());
        for c in PROTOCOL.iter().copied().chain(self.config.iter().map(String::as_str)) {
            all.push("-c");
            all.push(c);
        }
        all.extend_from_slice(args);
        let mut env: Vec<(&str, &str)> = vec![("GIT_CONFIG_NOSYSTEM", "1"), ("GIT_NO_LAZY_FETCH", "1")];
        env.extend(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        crate::org::git_output(&all, Some(&self.dir), &env)
    }

    /// The commit full ref `name` points to (tags peeled), read with `gix`.
    fn ref_commit(&self, name: &str) -> anyhow::Result<Option<String>> {
        let repo = gix::open(&self.dir).map_err(|e| anyhow::anyhow!("opening {}: {e}", self.dir.display()))?;
        let Some(mut r) = repo.try_find_reference(name).map_err(|e| anyhow::anyhow!("reading {name}: {e}"))? else {
            return Ok(None);
        };
        let commit = r.peel_to_commit().map_err(|e| anyhow::anyhow!("{name} does not point to a commit: {e}"))?;
        Ok(Some(commit.id.to_string()))
    }
}

/// `refs/rueca/commit/<sha>`.
fn commit_ref(sha: &str) -> String {
    format!("refs/rueca/commit/{sha}")
}

/// `git init --bare`, with automatic maintenance off.
fn init_bare(dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::org::git_output(&["init", "--bare", "--quiet"], Some(dir), &[("GIT_CONFIG_NOSYSTEM", "1")])?;
    let config = dir.join("config");
    let mut text = fs::read_to_string(&config).with_context(|| format!("reading {}", config.display()))?;
    text.push_str("[gc]\n\tauto = 0\n[maintenance]\n\tauto = false\n");
    fs::write(&config, text).with_context(|| format!("writing {}", config.display()))
}

/// Deletes the mirrors under `root` (directories with a
/// [`MIRROR_MARKER`]) not used for more than `max_age` at `now` (unix
/// seconds); returns them.
pub fn expire_mirrors(root: &Path, max_age: Duration, now: u64) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut removed = Vec::new();
    for dir in entries.flatten().map(|e| e.path()) {
        let Some(info) = MirrorInfo::read(&dir) else { continue };
        if now.saturating_sub(info.used_at) <= max_age.as_secs() {
            continue;
        }
        match fs::remove_dir_all(&dir) {
            Ok(()) => removed.push(dir),
            Err(e) => tracing::warn!(mirror = %dir.display(), error = %e, "cannot remove an expired mirror"),
        }
    }
    removed
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
