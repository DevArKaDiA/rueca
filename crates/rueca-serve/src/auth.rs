//! Authentication (RFC 0008 §6.3): the [`Authenticator`] seam, static API
//! tokens and the chain in front of `/mcp` and `/status`.
//!
//! Tokens come only in `Authorization: Bearer`. A token starting with
//! `rueca_` is a static token: its SHA-256 is compared in constant time
//! with every configured `sha256:<hex>` (no early exit), principal
//! `token:<name>`. A token with two dots is a JWT for the OIDC issuers
//! ([`crate::oidc`]), principal `oidc:<iss>#<sub>`. Anything else is
//! invalid. Code-host tokens are never accepted and no token is forwarded.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use http::request::Parts;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::{AuthConfig, StaticToken};
use crate::oidc::Oidc;

/// A boxed, sendable future (the seam returns one because OIDC may fetch a
/// JWKS).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Who is calling. `repos` are the repository-id globs the principal was
/// granted (static token `repos`, or the matching OIDC rules); the app turns
/// them into a [`rueca_query::Visibility`] per generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// `token:<name>` or `oidc:<iss>#<sub>`.
    pub id: String,
    pub scopes: BTreeSet<String>,
    pub groups: Vec<String>,
    pub repos: Vec<String>,
}

/// Why a request is not authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    /// No token: 401 with the challenge.
    Missing,
    /// A token that does not validate: 401 `invalid_token`, with a short
    /// description (never the token).
    Invalid(String),
    /// Valid, without the scope named: 403 `insufficient_scope`.
    InsufficientScope(String),
}

/// The message of an [`Challenge::Invalid`] for a JWT whose issuer is not
/// configured (metric reason `issuer`).
pub const UNKNOWN_ISSUER: &str = "token issuer is not accepted";

impl Challenge {
    /// The `reason` label of `rueca_auth_failures_total`.
    pub fn reason(&self) -> &'static str {
        match self {
            Challenge::Missing => "missing",
            Challenge::Invalid(m) if m == UNKNOWN_ISSUER => "issuer",
            Challenge::Invalid(_) => "invalid",
            Challenge::InsufficientScope(_) => "scope",
        }
    }
}

/// The authentication seam: Org mode's host-federated login plugs in here.
pub trait Authenticator: Send + Sync + 'static {
    fn authenticate<'a>(&'a self, parts: &'a Parts) -> BoxFuture<'a, Result<Principal, Challenge>>;
}

/// `sha256:<64 hex>` → digest.
pub fn parse_hash(text: &str) -> Option<[u8; 32]> {
    let hex = text.strip_prefix("sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `sha256:<hex>` of a token, as `[[auth.token]].hash` stores it.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

struct StaticEntry {
    hash: [u8; 32],
    token: StaticToken,
}

/// Static tokens, then OIDC.
pub struct AuthChain {
    statics: Vec<StaticEntry>,
    oidc: Option<Arc<Oidc>>,
}

impl AuthChain {
    pub fn new(config: &AuthConfig, default_audience: &str) -> anyhow::Result<AuthChain> {
        let statics = config
            .token
            .iter()
            .filter_map(|t| parse_hash(&t.hash).map(|hash| StaticEntry { hash, token: t.clone() }))
            .collect();
        let oidc =
            if config.oidc.is_empty() { None } else { Some(Arc::new(Oidc::new(&config.oidc, default_audience)?)) };
        Ok(AuthChain { statics, oidc })
    }

    /// The issuers PRM lists (empty: no OIDC, no PRM).
    pub fn issuers(&self) -> Vec<String> {
        self.oidc.as_ref().map(|o| o.issuers()).unwrap_or_default()
    }

    fn static_token(&self, token: &str) -> Result<Principal, Challenge> {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found: Option<&StaticEntry> = None;
        for entry in &self.statics {
            // Every entry is compared, in constant time, whatever matched.
            if bool::from(entry.hash.ct_eq(&digest)) {
                found = Some(entry);
            }
        }
        let entry = found.ok_or_else(|| Challenge::Invalid("unknown token".to_string()))?;
        Ok(Principal {
            id: format!("token:{}", entry.token.name),
            scopes: entry.token.scopes.iter().cloned().collect(),
            groups: Vec::new(),
            repos: entry.token.repos.clone(),
        })
    }

    async fn authenticate_token(&self, token: &str) -> Result<Principal, Challenge> {
        if token.starts_with("rueca_") {
            return self.static_token(token);
        }
        if token.split('.').count() == 3 {
            return match &self.oidc {
                Some(oidc) => oidc.authenticate(token).await,
                None => Err(Challenge::Invalid(UNKNOWN_ISSUER.to_string())),
            };
        }
        Err(Challenge::Invalid("malformed token".to_string()))
    }
}

impl Authenticator for AuthChain {
    fn authenticate<'a>(&'a self, parts: &'a Parts) -> BoxFuture<'a, Result<Principal, Challenge>> {
        Box::pin(async move {
            let token = bearer(parts)?;
            self.authenticate_token(token).await
        })
    }
}

/// The bearer token of `Authorization`.
pub fn bearer(parts: &Parts) -> Result<&str, Challenge> {
    let Some(value) = parts.headers.get(http::header::AUTHORIZATION) else { return Err(Challenge::Missing) };
    let value = value.to_str().map_err(|_| Challenge::Invalid("malformed Authorization header".to_string()))?;
    let (scheme, token) = value.trim().split_once(' ').unwrap_or((value, ""));
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(Challenge::Invalid("only Bearer tokens are accepted".to_string()));
    }
    let token = token.trim();
    if token.is_empty() {
        return Err(Challenge::Invalid("empty bearer token".to_string()));
    }
    Ok(token)
}

/// Whether `[auth].admins` grants `p` the admin role.
pub fn is_admin(p: &Principal, admins: &[String]) -> bool {
    admins.iter().any(|a| match a.split_once(':') {
        Some(("token", _)) => p.id == *a,
        Some(("group", g)) => p.groups.iter().any(|x| x == g),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(tokens: &[(&str, &str)]) -> AuthChain {
        let config = AuthConfig {
            token: tokens
                .iter()
                .map(|(name, secret)| StaticToken {
                    name: (*name).into(),
                    hash: hash_token(secret),
                    scopes: vec!["rueca:read".into()],
                    repos: vec!["github.com/acme/*".into()],
                })
                .collect(),
            ..AuthConfig::default()
        };
        AuthChain::new(&config, "http://localhost/mcp").unwrap()
    }

    fn parts(auth: Option<&str>) -> Parts {
        let mut b = http::Request::builder().uri("/mcp");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        b.body(()).unwrap().into_parts().0
    }

    #[test]
    fn hashes() {
        let h = hash_token("rueca_abc");
        assert!(h.starts_with("sha256:") && h.len() == 71);
        assert!(parse_hash(&h).is_some());
        assert!(parse_hash("sha256:00").is_none() && parse_hash("md5:00").is_none());
    }

    #[tokio::test]
    async fn static_tokens() {
        let c = chain(&[("ci", "rueca_one"), ("ops", "rueca_two")]);
        let p = c.authenticate(&parts(Some("Bearer rueca_two"))).await.unwrap();
        assert_eq!(p.id, "token:ops");
        assert!(p.scopes.contains("rueca:read"));
        assert_eq!(c.authenticate(&parts(None)).await, Err(Challenge::Missing));
        let bad = c.authenticate(&parts(Some("Bearer rueca_three"))).await.unwrap_err();
        assert_eq!(bad.reason(), "invalid");
        let basic = c.authenticate(&parts(Some("Basic abc"))).await.unwrap_err();
        assert!(matches!(basic, Challenge::Invalid(_)));
        let jwt = c.authenticate(&parts(Some("Bearer a.b.c"))).await.unwrap_err();
        assert_eq!(jwt.reason(), "issuer");
        let other = c.authenticate(&parts(Some("bearer ghp_codehosttoken"))).await.unwrap_err();
        assert_eq!(other, Challenge::Invalid("malformed token".into()));
    }

    #[test]
    fn admins() {
        let p =
            Principal { id: "token:ops".into(), scopes: BTreeSet::new(), groups: vec!["eng".into()], repos: vec![] };
        assert!(is_admin(&p, &["token:ops".into()]));
        assert!(is_admin(&p, &["group:eng".into()]));
        assert!(!is_admin(&p, &["group:platform".into(), "token:ci".into()]));
    }
}
