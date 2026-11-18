//! Talking to code hosts (RFC 0008 §7.4, §7.6, §9.2): the URL allowlist,
//! `git` with credentials from the environment, and `git ls-remote`.
//!
//! Every `git` run goes through the indexer's git runner
//! ([`rueca_index::org::git_output`]: `GIT_TERMINAL_PROMPT=0`, empty
//! `GIT_ASKPASS`) with `GIT_CONFIG_NOSYSTEM=1`, `protocol.allow=never` and
//! only `https` and `ssh` allowed (`file` and `git` too when
//! [`HostPolicy::allow_file_urls`] is set, for tests and the bench's local
//! `git daemon`). A host's credential becomes `http.extraHeader =
//! Authorization: Bearer <token>` passed as `GIT_CONFIG_COUNT` /
//! `GIT_CONFIG_KEY_0` / `GIT_CONFIG_VALUE_0` in the child's environment:
//! never on the command line, in a URL or in a repository's config.

use std::collections::BTreeMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};

/// How long a host that answered `429` is left alone: `git` does not show
/// the `Retry-After` header, so the pause is fixed.
pub const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);

/// Which repository URLs the service may contact (§9.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostPolicy {
    /// Allowed hosts (`github.com`), compared case-insensitively.
    pub hosts: Vec<String>,
    /// Addresses in loopback, link-local, private and metadata ranges are
    /// refused unless this is set.
    pub allow_private_networks: bool,
    /// `file://` URLs (a local bare repository as the "host") and `git://`
    /// URLs (a local `git daemon`, still subject to `hosts` and the private
    /// address check): tests and the bench harness only; the configuration
    /// accepts it only in debug builds.
    pub allow_file_urls: bool,
}

/// A repository URL split into what the policy checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// `https`, `ssh` or `file` (scp-like `git@host:path` is `ssh`).
    pub scheme: String,
    /// Lowercase host; empty for `file`.
    pub host: String,
}

impl RemoteUrl {
    pub fn parse(url: &str) -> anyhow::Result<RemoteUrl> {
        let url = url.trim();
        if !url.contains("://") {
            // scp-like `git@host:owner/name`.
            let Some((user_host, _)) = url.split_once(':').filter(|(h, _)| !h.contains('/')) else {
                bail!("`{url}` is not a repository URL");
            };
            let host = user_host.rsplit('@').next().unwrap_or(user_host);
            return Ok(RemoteUrl { scheme: "ssh".into(), host: host.to_ascii_lowercase() });
        }
        let parsed = url::Url::parse(url).with_context(|| format!("`{url}` is not a URL"))?;
        let host = parsed.host_str().unwrap_or("").trim_matches(['[', ']']).to_ascii_lowercase();
        Ok(RemoteUrl { scheme: parsed.scheme().to_string(), host })
    }
}

impl HostPolicy {
    /// Checks `url` without resolving its host: scheme, allowlist, and IP
    /// literals (or `localhost`) in private ranges.
    pub fn check(&self, url: &str) -> anyhow::Result<RemoteUrl> {
        let r = RemoteUrl::parse(url)?;
        match r.scheme.as_str() {
            "file" if self.allow_file_urls => return Ok(r),
            "file" | "git" if !self.allow_file_urls => {
                bail!("`{url}`: {} URLs are allowed only in tests (`[indexing].allow_file_urls`)", r.scheme)
            }
            "https" | "ssh" | "git" => {}
            other => bail!("`{url}`: scheme `{other}` is not allowed (https, ssh)"),
        }
        if !self.hosts.iter().any(|h| h.eq_ignore_ascii_case(&r.host)) {
            bail!("`{url}`: host `{}` is not in [indexing].hosts", r.host);
        }
        let literal = r.host.parse::<IpAddr>().ok();
        if !self.allow_private_networks && (r.host == "localhost" || literal.is_some_and(is_private)) {
            bail!("`{url}`: host `{}` is a private address (set allow_private_networks)", r.host);
        }
        Ok(r)
    }

    /// [`HostPolicy::check`], then resolves the host and refuses private
    /// addresses (unless allowed). Best effort: `git` resolves again.
    pub fn check_resolved(&self, url: &str) -> anyhow::Result<RemoteUrl> {
        let r = self.check(url)?;
        if r.scheme == "file" || self.allow_private_networks {
            return Ok(r);
        }
        let addrs = (r.host.as_str(), 443).to_socket_addrs().with_context(|| format!("resolving `{}`", r.host))?;
        if let Some(a) = addrs.map(|a| a.ip()).find(|ip| is_private(*ip)) {
            bail!("`{url}`: host `{}` resolves to the private address {a}", r.host);
        }
        Ok(r)
    }
}

/// Loopback, link-local, private, unique-local, unspecified and the
/// metadata address.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || v6.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
            }
        },
    }
}

/// The repository id of a URL: `host/owner/name` (as `rueca index` derives
/// it from an `origin` remote); a `file://` URL is `local/<name>`.
pub fn repo_id_of(url: &str) -> anyhow::Result<String> {
    if let Some(id) = rueca_index::normalize_remote(url) {
        return Ok(id);
    }
    let path = url.trim().strip_prefix("file://").with_context(|| format!("`{url}`: no repository id"))?;
    let name = Path::new(path.trim_end_matches('/'))
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.strip_suffix(".git").unwrap_or(n).to_string())
        .filter(|n| !n.is_empty())
        .with_context(|| format!("`{url}`: no repository name"))?;
    Ok(format!("local/{name}"))
}

/// How the service runs `git` against code hosts.
#[derive(Debug, Clone, Default)]
pub struct GitAccess {
    pub policy: HostPolicy,
    /// Host → bearer token (resolved `[indexing].credentials`).
    pub credentials: BTreeMap<String, String>,
}

/// One `git` invocation's protocol options and environment.
pub struct GitInvocation {
    pub config_args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl GitAccess {
    /// The `-c` options and environment for a `git` run against `url`.
    pub fn invocation(&self, url: &str) -> anyhow::Result<GitInvocation> {
        let r = self.policy.check(url)?;
        let mut config_args: Vec<String> =
            ["protocol.version=2", "protocol.allow=never", "protocol.https.allow=always"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        config_args.push("protocol.ssh.allow=always".into());
        if self.policy.allow_file_urls {
            config_args.push("protocol.file.allow=always".into());
            config_args.push("protocol.git.allow=always".into());
        }
        let mut env = vec![("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string())];
        if let Some(token) = self.credentials.get(&r.host) {
            env.push(("GIT_CONFIG_COUNT".into(), "1".into()));
            env.push(("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into()));
            env.push(("GIT_CONFIG_VALUE_0".into(), format!("Authorization: Bearer {token}")));
        }
        Ok(GitInvocation { config_args, env })
    }

    /// Runs `git -c … <args>` for `url` (in `cwd`), returning stdout.
    pub fn run(&self, url: &str, args: &[&str], cwd: Option<&Path>) -> anyhow::Result<String> {
        let inv = self.invocation(url)?;
        let mut all: Vec<&str> = Vec::with_capacity(inv.config_args.len() * 2 + args.len());
        for c in &inv.config_args {
            all.push("-c");
            all.push(c);
        }
        all.extend_from_slice(args);
        let env: Vec<(&str, &str)> = inv.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        rueca_index::org::git_output(&all, cwd, &env)
    }
}

/// What `git ls-remote --symref` listed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRefs {
    /// The branch `HEAD` points to (`main`), when the host says.
    pub head: Option<String>,
    /// Full ref name → commit (annotated tags peeled).
    pub refs: BTreeMap<String, String>,
}

impl RemoteRefs {
    /// Parses `git ls-remote --symref` output.
    pub fn parse(text: &str) -> RemoteRefs {
        let mut out = RemoteRefs::default();
        let mut peeled = Vec::new();
        for line in text.lines() {
            let Some((left, name)) = line.split_once('\t') else { continue };
            if let Some(target) = left.strip_prefix("ref: ") {
                if name == "HEAD" {
                    out.head = target.strip_prefix("refs/heads/").map(str::to_string);
                }
                continue;
            }
            match name.strip_suffix("^{}") {
                Some(tag) => peeled.push((tag.to_string(), left.to_string())),
                None => {
                    out.refs.insert(name.to_string(), left.to_string());
                }
            }
        }
        out.refs.extend(peeled);
        out
    }
}

/// Why listing a remote failed.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    /// The host answered `429` (or a secondary rate limit).
    #[error("rate limited by the host")]
    RateLimited { pause: Duration },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Lists a repository's refs: the network seam of the reconciler (tests
/// substitute a fake).
pub trait RemoteLister: Send + Sync {
    /// `git ls-remote --symref <url> <patterns…>`.
    fn ls_remote(&self, url: &str, patterns: &[&str]) -> Result<RemoteRefs, ListError>;
}

impl RemoteLister for GitAccess {
    fn ls_remote(&self, url: &str, patterns: &[&str]) -> Result<RemoteRefs, ListError> {
        self.policy.check_resolved(url)?;
        let mut args = vec!["ls-remote", "--symref", url];
        args.extend_from_slice(patterns);
        match self.run(url, &args, None) {
            Ok(text) => Ok(RemoteRefs::parse(&text)),
            Err(e) if is_rate_limited(&format!("{e:#}")) => Err(ListError::RateLimited { pause: RATE_LIMIT_PAUSE }),
            Err(e) => Err(ListError::Other(e)),
        }
    }
}

fn is_rate_limited(message: &str) -> bool {
    message.contains("error: 429") || message.contains(" 429 ") || message.contains("secondary rate limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> HostPolicy {
        HostPolicy { hosts: vec!["github.com".into(), "10.1.2.3".into()], ..HostPolicy::default() }
    }

    #[test]
    fn the_allowlist() {
        let p = policy();
        assert_eq!(p.check("https://GitHub.com/acme/x.git").unwrap().host, "github.com");
        assert_eq!(p.check("git@github.com:acme/x.git").unwrap().scheme, "ssh");
        let err = |url: &str| format!("{:#}", p.check(url).unwrap_err());
        assert!(err("https://gitlab.com/a/b").contains("not in [indexing].hosts"));
        assert!(err("http://github.com/a/b").contains("scheme `http`"));
        assert!(err("file:///tmp/x.git").contains("only in tests"));
        assert!(err("git://github.com/a/b.git").contains("only in tests"));
        assert!(err("https://10.1.2.3/a/b").contains("private address"));
        let open = HostPolicy { allow_private_networks: true, allow_file_urls: true, ..policy() };
        assert!(open.check("https://10.1.2.3/a/b").is_ok() && open.check("file:///tmp/x.git").is_ok());
        assert!(open.check("git://10.1.2.3:9418/a/b.git").is_ok(), "a local git daemon in tests");
        let no_private = HostPolicy { allow_file_urls: true, ..policy() };
        assert!(no_private.check("git://10.1.2.3/a/b.git").is_err(), "git:// still checks private addresses");
    }

    #[test]
    fn private_ranges() {
        for ip in ["127.0.0.1", "10.0.0.1", "172.16.5.4", "192.168.1.1", "169.254.169.254", "::1", "fd00::1", "fe80::1"]
        {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["140.82.112.3", "2606:50c0:8000::153"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn repo_ids() {
        assert_eq!(repo_id_of("https://github.com/acme/billing.git").unwrap(), "github.com/acme/billing");
        assert_eq!(repo_id_of("file:///tmp/host/billing.git").unwrap(), "local/billing");
    }

    #[test]
    fn ls_remote_output() {
        let text =
            "ref: refs/heads/main\tHEAD\naaa\tHEAD\naaa\trefs/heads/main\nbbb\trefs/tags/v2\naaa\trefs/tags/v2^{}\n";
        let r = RemoteRefs::parse(text);
        assert_eq!(r.head.as_deref(), Some("main"));
        assert_eq!(r.refs["refs/tags/v2"], "aaa", "annotated tags are peeled");
        assert_eq!(r.refs["refs/heads/main"], "aaa");
    }

    #[test]
    fn credentials_go_through_the_environment() {
        let mut access = GitAccess { policy: policy(), ..GitAccess::default() };
        access.credentials.insert("github.com".into(), "s3cret".into());
        let inv = access.invocation("https://github.com/acme/x.git").unwrap();
        assert!(inv.config_args.iter().all(|a| !a.contains("s3cret")));
        assert!(inv.env.iter().any(|(k, v)| k == "GIT_CONFIG_VALUE_0" && v == "Authorization: Bearer s3cret"));
        assert!(!access.invocation("git@github.com:a/b").unwrap().config_args.iter().any(|a| a.contains("file")));
    }
}
