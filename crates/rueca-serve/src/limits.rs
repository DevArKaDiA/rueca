//! Rate limits (RFC 0008 §6.5) and the request guards that run before
//! authentication: `Host` / `Origin` validation (§6.2) and the client IP.
//!
//! Limits use `governor` (GCRA) directly, keyed by client IP before
//! authentication and by principal id after it; per process (a global
//! limit belongs to the gateway). `tower_governor`'s `SmartIpKeyExtractor`
//! trusts forwarding headers from any peer, so the client IP is ours:
//! `X-Forwarded-For` (the rightmost address that is not a trusted proxy)
//! or `X-Real-IP`, only when the peer is in `[http].trusted_proxies`.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use http::HeaderMap;

use crate::config::{HttpConfig, RateLimit, cidr_contains, parse_cidr};

/// The two keyed limiters.
pub struct Limits {
    ip: DefaultKeyedRateLimiter<IpAddr>,
    principal: DefaultKeyedRateLimiter<String>,
    clock: DefaultClock,
    trusted: Vec<(IpAddr, u8)>,
}

fn quota(r: RateLimit) -> Quota {
    let per_second = NonZeroU32::new(r.per_second).unwrap_or(NonZeroU32::MIN);
    let burst = NonZeroU32::new(r.burst).unwrap_or(NonZeroU32::MIN);
    Quota::per_second(per_second).allow_burst(burst)
}

impl Limits {
    pub fn new(http: &HttpConfig) -> Limits {
        Limits {
            ip: RateLimiter::keyed(quota(http.ip_rate_limit)),
            principal: RateLimiter::keyed(quota(http.rate_limit)),
            clock: DefaultClock::default(),
            trusted: http.trusted_proxies.iter().filter_map(|p| parse_cidr(p)).collect(),
        }
    }

    /// `Err(wait)` when `ip` is over its limit.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), Duration> {
        self.ip.check_key(&ip).map_err(|n| n.wait_time_from(self.clock.now()))
    }

    /// `Err(wait)` when principal `id` is over its limit.
    pub fn check_principal(&self, id: &str) -> Result<(), Duration> {
        self.principal.check_key(&id.to_string()).map_err(|n| n.wait_time_from(self.clock.now()))
    }

    /// Forgets keys that are back to a full bucket (called periodically).
    pub fn retain_recent(&self) {
        self.ip.retain_recent();
        self.principal.retain_recent();
    }

    fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|&(net, len)| cidr_contains(net, len, ip))
    }

    /// The client address of a request from `peer`.
    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if !self.is_trusted(peer) {
            return peer;
        }
        let forwarded: Vec<IpAddr> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if let Some(ip) = forwarded.iter().rev().find(|ip| !self.is_trusted(**ip)) {
            return *ip;
        }
        if let Some(first) = forwarded.first() {
            return *first;
        }
        headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse().ok()).unwrap_or(peer)
    }
}

/// `Host` and `Origin` validation (DNS rebinding, cross-site requests).
pub struct HostGuard {
    /// `(host, port)`: `None` port matches any.
    hosts: Vec<(String, Option<u16>)>,
    origins: Vec<String>,
    /// Neither `allowed_hosts` nor `canonical_url`: loopback only.
    loopback: bool,
}

const LOOPBACK: &[&str] = &["localhost", "127.0.0.1", "[::1]", "::1"];

impl HostGuard {
    pub fn new(http: &HttpConfig) -> HostGuard {
        let mut hosts: Vec<(String, Option<u16>)> = http.allowed_hosts.iter().map(|h| split_host(h)).collect();
        let mut origins: Vec<String> = http.allowed_origins.iter().filter_map(|o| origin_of(o)).collect();
        let canonical = http.canonical_url.as_deref().and_then(|u| url::Url::parse(u).ok());
        if let Some(u) = &canonical {
            if let Some(h) = u.host_str() {
                hosts.push((h.to_ascii_lowercase(), u.port()));
            }
            origins.push(u.origin().ascii_serialization());
        }
        HostGuard { loopback: hosts.is_empty(), hosts, origins }
    }

    /// `Err(why)` when the request must be refused with 403.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        let host = headers.get(http::header::HOST).and_then(|v| v.to_str().ok()).ok_or("missing Host header")?;
        let (name, port) = split_host(host);
        let host_ok = if self.loopback {
            LOOPBACK.contains(&name.as_str())
        } else {
            self.hosts.iter().any(|(h, p)| *h == name && p.is_none_or(|p| Some(p) == port))
        };
        if !host_ok {
            return Err("Host is not allowed");
        }
        let Some(origin) = headers.get(http::header::ORIGIN) else { return Ok(()) };
        let origin = origin.to_str().ok().and_then(origin_of).ok_or("Origin is not allowed")?;
        let loopback_origin = self.loopback
            && url::Url::parse(&origin)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .is_some_and(|h| LOOPBACK.contains(&h.as_str()));
        if loopback_origin || self.origins.contains(&origin) { Ok(()) } else { Err("Origin is not allowed") }
    }
}

/// `host[:port]` → (lowercase host, port).
fn split_host(text: &str) -> (String, Option<u16>) {
    let t = text.trim().to_ascii_lowercase();
    if let Some(rest) = t.strip_prefix('[')
        && let Some((h, after)) = rest.split_once(']')
    {
        return (format!("[{h}]"), after.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match t.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(port) => (h.to_string(), Some(port)),
            Err(_) => (t.clone(), None),
        },
        _ => (t.clone(), None),
    }
}

/// The ASCII serialization of an origin (`scheme://host[:port]`).
fn origin_of(text: &str) -> Option<String> {
    let u = url::Url::parse(text.trim()).ok()?;
    u.host_str()?;
    Some(u.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    fn http_config(text: &str) -> HttpConfig {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn loopback_by_default() {
        let g = HostGuard::new(&HttpConfig::default());
        assert!(g.check(&headers(&[("host", "127.0.0.1:8080")])).is_ok());
        assert!(g.check(&headers(&[("host", "localhost"), ("origin", "http://localhost:3000")])).is_ok());
        assert!(g.check(&headers(&[("host", "[::1]:9")])).is_ok());
        assert!(g.check(&headers(&[("host", "evil.example")])).is_err());
        assert!(g.check(&headers(&[("host", "localhost"), ("origin", "https://evil.example")])).is_err());
        assert!(g.check(&headers(&[])).is_err());
    }

    #[test]
    fn canonical_and_allowed() {
        let g = HostGuard::new(&http_config(
            r#"
canonical_url = "https://rueca.acme.internal/mcp"
allowed_hosts = ["10.0.0.5:8080"]
allowed_origins = ["https://ide.acme.internal"]
"#,
        ));
        assert!(g.check(&headers(&[("host", "rueca.acme.internal")])).is_ok());
        assert!(g.check(&headers(&[("host", "RUECA.acme.internal:443")])).is_ok());
        assert!(g.check(&headers(&[("host", "10.0.0.5:8080")])).is_ok());
        assert!(g.check(&headers(&[("host", "10.0.0.5:9090")])).is_err());
        assert!(g.check(&headers(&[("host", "localhost")])).is_err());
        let ok = [("host", "rueca.acme.internal"), ("origin", "https://rueca.acme.internal")];
        assert!(g.check(&headers(&ok)).is_ok());
        assert!(g.check(&headers(&[("host", "rueca.acme.internal"), ("origin", "https://ide.acme.internal")])).is_ok());
        assert!(g.check(&headers(&[("host", "rueca.acme.internal"), ("origin", "null")])).is_err());
    }

    #[test]
    fn client_ip_behind_trusted_proxies() {
        let l = Limits::new(&http_config(r#"trusted_proxies = ["10.0.0.0/8"]"#));
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let xff = headers(&[("x-forwarded-for", "203.0.113.9, 198.51.100.2, 10.1.1.1")]);
        assert_eq!(l.client_ip(ip("10.0.0.1"), &xff), ip("198.51.100.2"));
        assert_eq!(l.client_ip(ip("192.0.2.1"), &xff), ip("192.0.2.1"), "untrusted peer: headers ignored");
        assert_eq!(l.client_ip(ip("10.0.0.1"), &headers(&[("x-real-ip", "203.0.113.7")])), ip("203.0.113.7"));
    }

    #[test]
    fn limits_refuse_over_burst() {
        let l = Limits::new(&http_config("rate_limit = { per_second = 1, burst = 2 }"));
        assert!(l.check_principal("token:a").is_ok() && l.check_principal("token:a").is_ok());
        let wait = l.check_principal("token:a").unwrap_err();
        assert!(wait <= Duration::from_secs(1), "{wait:?}");
        assert!(l.check_principal("token:b").is_ok(), "per principal");
    }
}
