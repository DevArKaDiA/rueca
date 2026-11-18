//! Sending presigned requests with `ureq`, retries, replies and S3 errors.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use rusty_s3::S3Action;
use ureq::Agent;
use ureq::http::{Method, Request, Response};
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};
use url::Url;

use super::config::CredentialsProvider;

/// Signed request headers: lowercase name, value.
pub(crate) type Headers = Vec<(&'static str, String)>;

/// How long a presigned URL stays valid: long enough for an 8 MiB part on a
/// slow link and for retries.
const PRESIGN_TTL: Duration = Duration::from_secs(15 * 60);
/// Attempts per request for transport errors, 5xx, 429 and 409.
const ATTEMPTS: u32 = 3;
const RETRY_BASE: Duration = Duration::from_millis(100);

/// Signs `action` as a presigned URL whose signed headers are `headers`
/// (which the request must then send with the same values).
pub(crate) fn presign<'a, A: S3Action<'a>>(mut action: A, headers: &'a [(&'static str, String)]) -> Url {
    for (name, value) in headers {
        action.headers_mut().insert(*name, value.as_str());
    }
    action.sign(PRESIGN_TTL)
}

/// Builds the URL to send, given the credentials and the signed headers.
pub(crate) type Signer<'s> = dyn Fn(Option<&rusty_s3::Credentials>, &[(&'static str, String)]) -> Url + 's;

pub(crate) struct Client {
    agent: Agent,
    credentials: Option<Arc<dyn CredentialsProvider>>,
}

/// A response whose status the caller interprets.
pub(crate) struct Reply(Response<ureq::Body>);

impl Client {
    pub fn new(https: bool, credentials: Option<Arc<dyn CredentialsProvider>>) -> anyhow::Result<Client> {
        let roots = if https { native_roots()? } else { Vec::new() };
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::from(roots))
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .build()
            .new_agent();
        Ok(Client { agent, credentials })
    }

    fn rusty_credentials(&self) -> anyhow::Result<Option<rusty_s3::Credentials>> {
        let Some(provider) = &self.credentials else { return Ok(None) };
        let c = provider.credentials()?;
        Ok(Some(match c.session_token {
            Some(token) => rusty_s3::Credentials::new_with_token(c.access_key_id, c.secret_access_key, token),
            None => rusty_s3::Credentials::new(c.access_key_id, c.secret_access_key),
        }))
    }

    /// Sends `method` to the URL `sign` builds, with `headers`, retrying
    /// transport errors and retryable statuses; re-signs every attempt.
    pub fn execute(
        &self,
        method: Method,
        headers: &Headers,
        body: Option<&[u8]>,
        sign: &Signer<'_>,
    ) -> anyhow::Result<Reply> {
        let mut attempt = 0;
        loop {
            let credentials = self.rusty_credentials()?;
            let url = sign(credentials.as_ref(), headers);
            let last = attempt + 1 >= ATTEMPTS;
            match self.send(&method, &url, headers, body) {
                Ok(reply) if last || !retryable(reply.status()) => return Ok(reply),
                Ok(reply) => tracing::debug!(%method, status = reply.status(), attempt, "retrying"),
                Err(e) if last || !transient(&e) => {
                    return Err(anyhow!(e)).with_context(|| format!("{method} {}", redact(&url)));
                }
                Err(e) => tracing::debug!(%method, error = %e, attempt, "retrying"),
            }
            std::thread::sleep(RETRY_BASE * 2u32.pow(attempt));
            attempt += 1;
        }
    }

    fn send(&self, method: &Method, url: &Url, headers: &Headers, body: Option<&[u8]>) -> Result<Reply, ureq::Error> {
        let mut builder = Request::builder().method(method.clone()).uri(url.as_str());
        for (name, value) in headers {
            builder = builder.header(*name, value.as_str());
        }
        let response = match body {
            Some(bytes) => self.agent.run(builder.body(bytes)?),
            None => self.agent.run(builder.body(())?),
        }?;
        Ok(Reply(response))
    }
}

impl Reply {
    pub fn status(&self) -> u16 {
        self.0.status().as_u16()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.0.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    }

    pub fn bytes(mut self) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.body_mut().with_config().limit(u64::MAX).read_to_vec()?)
    }

    pub fn text(self) -> anyhow::Result<String> {
        Ok(String::from_utf8(self.bytes()?)?)
    }

    pub fn into_reader(self) -> impl std::io::Read {
        self.0.into_body().into_reader()
    }

    /// An error naming the operation, the key, the status and S3's error code
    /// and message when the body has them.
    pub fn error(self, op: &str, key: &str) -> anyhow::Error {
        let status = self.status();
        let body = self.text().unwrap_or_default();
        let code = xml_tag(&body, "Code").unwrap_or_default();
        let message = xml_tag(&body, "Message").unwrap_or_default();
        anyhow!("{op} {key}: HTTP {status} {code} {message}")
    }
}

/// The text of the first `<tag>…</tag>` in `xml` (S3 error bodies are flat).
pub(crate) fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(xml[start..end].to_string())
}

fn retryable(status: u16) -> bool {
    matches!(status, 409 | 429 | 500 | 502 | 503 | 504)
}

fn transient(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::HostNotFound
    )
}

/// The URL without its query string (which carries the signature).
fn redact(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.to_string()
}

/// The platform's root certificates (`rustls-native-certs`).
fn native_roots() -> anyhow::Result<Vec<Certificate<'static>>> {
    let found = rustls_native_certs::load_native_certs();
    if found.certs.is_empty() {
        anyhow::bail!("no root certificates found on this system: {:?}", found.errors);
    }
    for e in &found.errors {
        tracing::debug!(error = %e, "skipping an unreadable root certificate");
    }
    Ok(found.certs.iter().map(|c| Certificate::from_der(c.as_ref()).to_owned()).collect())
}

#[cfg(test)]
mod tests {
    use rusty_s3::{Bucket, UrlStyle};

    use super::*;

    #[test]
    fn conditional_headers_are_signed() {
        let endpoint = "https://s3.us-east-1.amazonaws.com".parse().unwrap();
        let bucket = Bucket::new(endpoint, UrlStyle::Path, "b", "us-east-1").unwrap();
        let creds = rusty_s3::Credentials::new("AKIAIOSFODNN7EXAMPLE", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
        let headers: Headers = vec![("if-none-match", "*".to_string())];
        let url = presign(bucket.put_object(Some(&creds), "p/k"), &headers);
        let query = url.query().unwrap();
        assert!(query.contains("X-Amz-SignedHeaders=host%3Bif-none-match"), "{query}");
        assert!(query.contains("X-Amz-Signature="), "{query}");
        assert_eq!(url.path(), "/b/p/k");
        assert_eq!(redact(&url), "https://s3.us-east-1.amazonaws.com/b/p/k");
    }

    #[test]
    fn error_bodies() {
        let body =
            "<?xml version=\"1.0\"?><Error><Code>PreconditionFailed</Code><Message>At least one</Message></Error>";
        assert_eq!(xml_tag(body, "Code").as_deref(), Some("PreconditionFailed"));
        assert_eq!(xml_tag(body, "Message").as_deref(), Some("At least one"));
        assert_eq!(xml_tag(body, "Missing"), None);
    }
}
