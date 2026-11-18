//! (b) OIDC access tokens against a fake issuer served in-process: the
//! discovery document and JWKS over HTTP, ES256 tokens minted with
//! `jsonwebtoken`; PRM and the `WWW-Authenticate` challenges.

mod common;

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::routing::get as route_get;
use common::{Server, get, populated};
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

/// A P-256 private key as PKCS#8 DER (no public key: derived by the
/// backend), from a fixed scalar.
fn es256_key() -> EncodingKey {
    let mut der = vec![
        0x30, 0x41, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x04, 0x27, 0x30, 0x25, 0x02, 0x01, 0x01, 0x04, 0x20,
    ];
    der.extend((1..=32u8).map(|b| b.wrapping_mul(7)));
    EncodingKey::from_ec_der(&der)
}

/// Serves `/.well-known/openid-configuration` and `/jwks` on an ephemeral
/// port of `rt`; returns the issuer URL.
fn fake_issuer(rt: &tokio::runtime::Runtime, key: &EncodingKey) -> String {
    let mut jwk = Jwk::from_encoding_key(key, Algorithm::ES256).unwrap();
    jwk.common.key_id = Some("k1".into());
    let jwks = serde_json::to_value(JwkSet { keys: vec![jwk] }).unwrap();
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let issuer = format!("http://{addr}");
    let discovery = json!({"issuer": issuer, "jwks_uri": format!("{issuer}/jwks")});
    let app = Router::new()
        .route("/.well-known/openid-configuration", route_get(move || async move { axum::Json(discovery) }))
        .route("/jwks", route_get(move || async move { axum::Json(jwks) }));
    rt.spawn(async move { axum::serve(listener, app).await.unwrap() });
    issuer
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn mint(key: &EncodingKey, claims: Value) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("k1".into());
    jsonwebtoken::encode(&header, &claims, key).unwrap()
}

/// Mints ES256 tokens for the fake issuer: default claims (subject `ana`,
/// group `eng`, scope `rueca:read`, valid ten minutes) overridden by `extra`.
struct Minter {
    key: EncodingKey,
    issuer: String,
    audience: &'static str,
}

impl Minter {
    fn token(&self, extra: Value) -> String {
        let mut c = json!({"iss": self.issuer, "sub": "ana", "aud": self.audience, "exp": now() + 600, "iat": now(),
            "scope": "openid rueca:read", "groups": ["eng"]});
        for (k, v) in extra.as_object().into_iter().flatten() {
            c[k] = v.clone();
        }
        mint(&self.key, c)
    }
}

const PRM_URL: &str = "http://rueca.test/.well-known/oauth-protected-resource/mcp";

fn list() -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
}

#[test]
fn oidc_tokens_prm_and_challenges() {
    let _p = populated("memory://serve-b");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let key = es256_key();
    let issuer = fake_issuer(&rt, &key);
    let m = Minter { key, issuer, audience: "http://rueca.test/mcp" };
    let extra = format!(
        r#"canonical_url = "{}"
allowed_hosts = ["127.0.0.1"]

[[auth.oidc]]
issuer = "{}"
algorithms = ["ES256"]
rules = [{{ group = "eng", repos = ["github.com/acme/*"] }}]
"#,
        m.audience, m.issuer
    );
    let s = Server::start("memory://serve-b", &extra);
    s.wait_ready();
    no_token_names_the_prm(&s, &m);
    accepted_tokens_see_their_rules(&s, &m);
    rejected_tokens(&s, &m);
    let metrics = get(&format!("{}/metrics", s.ops), &[]).body;
    for reason in ["missing", "invalid", "issuer", "scope"] {
        assert!(metrics.contains(&format!("rueca_auth_failures_total{{reason=\"{reason}\"}}")), "{reason}: {metrics}");
    }
    drop(s);
    rt.shutdown_background();
}

/// No token: 401, the challenge names the PRM, served at both paths.
fn no_token_names_the_prm(s: &Server, m: &Minter) {
    let r = s.mcp(None, &list(), &[]);
    assert_eq!(r.status, 401);
    assert_eq!(
        r.header("www-authenticate").unwrap(),
        format!("Bearer scope=\"rueca:read\", resource_metadata=\"{PRM_URL}\"")
    );
    for path in ["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource"] {
        let prm = get(&format!("{}{path}", s.base), &[]);
        assert_eq!(prm.status, 200, "{}", prm.body);
        let v = prm.json();
        assert_eq!(v["resource"], m.audience);
        assert_eq!(v["authorization_servers"], json!([m.issuer]));
        assert_eq!(v["bearer_methods_supported"], json!(["header"]));
        assert_eq!(v["resource_name"], "Rueca (acme)");
    }
}

/// A valid token reads what its group's rule allows; a group without a rule
/// is authenticated and sees nothing.
fn accepted_tokens_see_their_rules(s: &Server, m: &Minter) {
    let r = s.call(&m.token(json!({})), "list_repositories", json!({}));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("github.com/acme/billing") && !r.body.contains("hidden/secret"), "{}", r.body);
    let r = s.call(&m.token(json!({"groups": ["sales"]})), "list_repositories", json!({}));
    assert_eq!(r.status, 200);
    assert!(!r.body.contains("github.com/acme"), "{}", r.body);
}

/// Expired, wrong audience, another issuer: 401; without `rueca:read`: 403.
fn rejected_tokens(s: &Server, m: &Minter) {
    let r = s.mcp(Some(&m.token(json!({"exp": now() - 3600}))), &list(), &[]);
    assert_eq!(r.status, 401);
    assert_eq!(
        r.header("www-authenticate").unwrap(),
        format!("Bearer error=\"invalid_token\", error_description=\"token expired\", resource_metadata=\"{PRM_URL}\"")
    );
    let r = s.mcp(Some(&m.token(json!({"aud": "https://elsewhere/mcp"}))), &list(), &[]);
    assert_eq!(r.status, 401);
    assert!(r.header("www-authenticate").unwrap().contains("audience"), "{:?}", r.header("www-authenticate"));
    assert_eq!(s.mcp(Some(&m.token(json!({"iss": "https://idp.example"}))), &list(), &[]).status, 401);
    let r = s.mcp(Some(&m.token(json!({"scope": "openid"}))), &list(), &[]);
    assert_eq!(r.status, 403);
    assert_eq!(
        r.header("www-authenticate").unwrap(),
        format!("Bearer error=\"insufficient_scope\", scope=\"rueca:read\", resource_metadata=\"{PRM_URL}\"")
    );
}
