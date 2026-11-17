//! `s3://` and `gs://`: one S3-compatible client. Requests are built and
//! signed by `rusty-s3` (presigned SigV4 URLs; the conditional headers are
//! signed headers) and sent by `ureq` over rustls with the platform's roots.

mod client;
mod config;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, anyhow, bail};
use bytes::Bytes;
use rusty_s3::actions::{CompleteMultipartUploadResponse, CreateMultipartUpload, ListObjectsV2};
use rusty_s3::{Bucket, UrlStyle};
use ureq::http::Method;

use client::{Client, Headers, Reply, presign};
pub use config::{Credentials, CredentialsProvider, Flavour, StaticCredentials};
use config::{S3Config, Settings};

use crate::{ObjectMeta, ObjectVersion, PutError, Storage, Versioned, check_path};

/// Files above this size are uploaded in parts of this size.
const PART_SIZE: usize = 8 * 1024 * 1024;

/// An S3-compatible bucket (S3 or GCS's XML API), optionally under a key
/// prefix.
pub struct S3Storage {
    cfg: S3Config,
    bucket: Bucket,
    client: Client,
}

/// What a `GetObject` found.
enum Fetched {
    Body(Reply),
    NotModified,
    Missing,
}

impl S3Storage {
    /// Opens `s3://` / `gs://` store `rest` (`bucket[/prefix]`).
    ///
    /// `options` (an `aws_` or `google_` prefix is accepted on every key),
    /// falling back to the environment:
    ///
    /// | Option | Environment | Default |
    /// |---|---|---|
    /// | `access_key_id`, `secret_access_key` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`; GCS: `GOOGLE_HMAC_KEY_ID`, `GOOGLE_HMAC_SECRET` first | none: unsigned requests |
    /// | `session_token` | `AWS_SESSION_TOKEN` (S3) | none |
    /// | `region` | `AWS_REGION`, `AWS_DEFAULT_REGION` (S3) | `us-east-1`; GCS `auto` |
    /// | `endpoint` | `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT_URL` (S3) | `https://s3.<region>.amazonaws.com`; GCS `https://storage.googleapis.com` |
    /// | `allow_http` | `AWS_ALLOW_HTTP` | `false`: an `http://` endpoint is an error |
    /// | `virtual_hosted_style_request` | `AWS_VIRTUAL_HOSTED_STYLE_REQUEST` | `true` on AWS's endpoint, else path style |
    ///
    /// Unknown option keys are an error.
    pub fn open(flavour: Flavour, rest: &str, options: &BTreeMap<String, String>) -> anyhow::Result<S3Storage> {
        let env = |name: &str| std::env::var(name).ok();
        let settings = Settings::new(options, &env)?;
        Self::from_config(S3Config::resolve(flavour, rest, &settings)?)
    }

    /// [`S3Storage::open`] with credentials from `provider` instead of the
    /// options and environment.
    pub fn with_credentials(
        flavour: Flavour,
        rest: &str,
        options: &BTreeMap<String, String>,
        provider: Arc<dyn CredentialsProvider>,
    ) -> anyhow::Result<S3Storage> {
        let env = |name: &str| std::env::var(name).ok();
        let settings = Settings::new(options, &env)?;
        let mut cfg = S3Config::resolve(flavour, rest, &settings)?;
        cfg.credentials = Some(provider);
        Self::from_config(cfg)
    }

    fn from_config(cfg: S3Config) -> anyhow::Result<S3Storage> {
        let endpoint = cfg.endpoint.parse().with_context(|| format!("store endpoint `{}`", cfg.endpoint))?;
        let style = if cfg.virtual_hosted { UrlStyle::VirtualHost } else { UrlStyle::Path };
        let bucket = Bucket::new(endpoint, style, cfg.bucket.clone(), cfg.region.clone())
            .with_context(|| format!("bucket `{}` at `{}`", cfg.bucket, cfg.endpoint))?;
        let credentials = cfg.credentials.clone();
        let client = Client::new(cfg.endpoint.starts_with("https://"), credentials)?;
        Ok(S3Storage { cfg, bucket, client })
    }

    fn key(&self, path: &str) -> anyhow::Result<String> {
        check_path(path)?;
        Ok(format!("{}{path}", self.cfg.prefix))
    }

    fn version_of(&self, reply: &Reply) -> ObjectVersion {
        let version_header = match self.cfg.flavour {
            Flavour::S3 => "x-amz-version-id",
            Flavour::Gcs => "x-goog-generation",
        };
        ObjectVersion { e_tag: reply.header("etag"), version: reply.header(version_header) }
    }

    /// The signed header that makes a write conditional.
    fn write_condition(&self, expected: Option<&ObjectVersion>) -> anyhow::Result<Headers> {
        let header = match (self.cfg.flavour, expected) {
            (Flavour::S3, None) => ("if-none-match", "*".to_string()),
            (Flavour::Gcs, None) => ("x-goog-if-generation-match", "0".to_string()),
            (Flavour::S3, Some(v)) => ("if-match", v.e_tag.clone().context("update without an e-tag")?),
            (Flavour::Gcs, Some(v)) => {
                ("x-goog-if-generation-match", v.version.clone().context("GCS update without a generation")?)
            }
        };
        Ok(vec![header])
    }

    /// The signed header that makes a read conditional on a change.
    fn read_condition(&self, known: &ObjectVersion) -> Headers {
        match (self.cfg.flavour, &known.version, &known.e_tag) {
            (Flavour::Gcs, Some(generation), _) => {
                vec![("x-goog-if-generation-not-match", generation.clone())]
            }
            (_, _, Some(e_tag)) => vec![("if-none-match", e_tag.clone())],
            _ => Vec::new(),
        }
    }

    fn fetch(&self, key: &str, headers: &Headers) -> anyhow::Result<Fetched> {
        let reply =
            self.client.execute(Method::GET, headers, None, &|c, h| presign(self.bucket.get_object(c, key), h))?;
        match reply.status() {
            200 | 206 => Ok(Fetched::Body(reply)),
            304 | 412 => Ok(Fetched::NotModified),
            404 => Ok(Fetched::Missing),
            _ => Err(reply.error("GetObject", key)),
        }
    }

    fn put_object(&self, key: &str, body: &[u8], expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        let headers = self.write_condition(expected)?;
        let reply = self
            .client
            .execute(Method::PUT, &headers, Some(body), &|c, h| presign(self.bucket.put_object(c, key), h))?;
        match reply.status() {
            200 | 201 | 204 => Ok(self.version_of(&reply)),
            status => Err(put_failure(status, expected.is_none(), reply.error("PutObject", key))),
        }
    }

    fn multipart(&self, key: &str, src: &mut File) -> Result<ObjectVersion, PutError> {
        let upload_id = self.create_upload(key)?;
        let result = self
            .upload_parts(key, &upload_id, src)
            .map_err(PutError::Other)
            .and_then(|e_tags| self.complete_upload(key, &upload_id, &e_tags));
        if result.is_err()
            && let Err(e) = self.abort_upload(key, &upload_id)
        {
            tracing::warn!(key, error = %e, "aborting a failed multipart upload failed");
        }
        result
    }

    fn create_upload(&self, key: &str) -> anyhow::Result<String> {
        let reply = self.client.execute(Method::POST, &Vec::new(), Some(&[]), &|c, h| {
            presign(self.bucket.create_multipart_upload(c, key), h)
        })?;
        if reply.status() != 200 {
            return Err(reply.error("CreateMultipartUpload", key));
        }
        let body = reply.text()?;
        let parsed = CreateMultipartUpload::parse_response(&body)
            .map_err(|e| anyhow!("CreateMultipartUpload {key}: unexpected response ({e:?}): {body}"))?;
        Ok(parsed.upload_id().to_string())
    }

    fn upload_parts(&self, key: &str, upload_id: &str, src: &mut File) -> anyhow::Result<Vec<String>> {
        let mut e_tags = Vec::new();
        let mut buf = vec![0u8; PART_SIZE];
        loop {
            let n = read_full(src, &mut buf)?;
            if n == 0 {
                break;
            }
            let number = u16::try_from(e_tags.len() + 1).context("more than 65535 parts")?;
            let reply = self.client.execute(Method::PUT, &Vec::new(), Some(&buf[..n]), &|c, h| {
                presign(self.bucket.upload_part(c, key, number, upload_id), h)
            })?;
            if reply.status() != 200 {
                return Err(reply.error("UploadPart", key));
            }
            e_tags.push(reply.header("etag").with_context(|| format!("UploadPart {key}: no ETag"))?);
        }
        Ok(e_tags)
    }

    fn complete_upload(&self, key: &str, upload_id: &str, e_tags: &[String]) -> Result<ObjectVersion, PutError> {
        let headers = self.write_condition(None)?;
        let parts = || e_tags.iter().map(String::as_str);
        let body = self.bucket.complete_multipart_upload(None, key, upload_id, parts()).body();
        let reply = self.client.execute(Method::POST, &headers, Some(body.as_bytes()), &|c, h| {
            presign(self.bucket.complete_multipart_upload(c, key, upload_id, parts()), h)
        })?;
        let status = reply.status();
        let version = self.version_of(&reply);
        if status != 200 {
            return Err(put_failure(status, true, reply.error("CompleteMultipartUpload", key)));
        }
        // S3 can answer 200 with an <Error> body.
        let text = reply.text()?;
        match CompleteMultipartUploadResponse::parse(&text) {
            Ok(done) => Ok(ObjectVersion { e_tag: Some(done.etag().to_string()), ..version }),
            Err(_) if client::xml_tag(&text, "Code").as_deref() == Some("PreconditionFailed") => {
                Err(PutError::AlreadyExists)
            }
            Err(_) => Err(PutError::Other(anyhow!("CompleteMultipartUpload {key}: {text}"))),
        }
    }

    fn abort_upload(&self, key: &str, upload_id: &str) -> anyhow::Result<()> {
        let reply = self.client.execute(Method::DELETE, &Vec::new(), None, &|c, h| {
            presign(self.bucket.abort_multipart_upload(c, key, upload_id), h)
        })?;
        match reply.status() {
            200 | 204 | 404 => Ok(()),
            _ => Err(reply.error("AbortMultipartUpload", key)),
        }
    }

    fn list_page(&self, prefix: &str, token: Option<&str>) -> anyhow::Result<rusty_s3::actions::ListObjectsV2Response> {
        let reply = self.client.execute(Method::GET, &Vec::new(), None, &|c, h| {
            let mut action = self.bucket.list_objects_v2(c);
            if !prefix.is_empty() {
                action.with_prefix(prefix);
            }
            if let Some(token) = token {
                action.with_continuation_token(token);
            }
            presign(action, h)
        })?;
        if reply.status() != 200 {
            return Err(reply.error("ListObjectsV2", prefix));
        }
        parse_list(&reply.text()?)
    }

    fn object_meta(&self, content: rusty_s3::actions::list_objects_v2::ListObjectsContent) -> Option<ObjectMeta> {
        let path = content.key.strip_prefix(&self.cfg.prefix)?.to_string();
        Some(ObjectMeta {
            path,
            size: content.size.unwrap_or(0),
            version: ObjectVersion { e_tag: content.etag, version: None },
            last_modified: content.last_modified.as_deref().and_then(parse_timestamp),
        })
    }
}

/// Maps a failed conditional write's status.
fn put_failure(status: u16, create: bool, error: anyhow::Error) -> PutError {
    match (status, create) {
        (412, true) => PutError::AlreadyExists,
        (412 | 404 | 409, false) => PutError::Precondition,
        (409, true) => PutError::AlreadyExists,
        _ => PutError::Other(error),
    }
}

/// `ListObjectsV2` response; GCS answers in its own XML namespace, which
/// `rusty-s3`'s parser does not accept, so it is rewritten to S3's.
fn parse_list(body: &str) -> anyhow::Result<rusty_s3::actions::ListObjectsV2Response> {
    const S3_NS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";
    const GCS_NS: &str = "http://doc.s3.amazonaws.com/2006-03-01";
    match ListObjectsV2::parse_response(body) {
        Ok(parsed) => Ok(parsed),
        Err(_) if body.contains(GCS_NS) => ListObjectsV2::parse_response(&body.replace(GCS_NS, S3_NS))
            .map_err(|e| anyhow!("ListObjectsV2: unexpected response ({e:?})")),
        Err(e) => bail!("ListObjectsV2: unexpected response ({e:?})"),
    }
}

/// `2009-10-12T17:50:30.000Z` → `SystemTime`.
fn parse_timestamp(s: &str) -> Option<SystemTime> {
    s.parse::<jiff::Timestamp>().ok().map(SystemTime::from)
}

/// Reads until `buf` is full or the input ends; the number of bytes read.
fn read_full(input: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

impl Storage for S3Storage {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        let key = self.key(path)?;
        match self.fetch(&key, &Vec::new())? {
            Fetched::Body(reply) => {
                let version = self.version_of(&reply);
                Ok(Some(Versioned { value: Bytes::from(reply.bytes()?), version }))
            }
            Fetched::NotModified | Fetched::Missing => Ok(None),
        }
    }

    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        let key = self.key(path)?;
        match self.fetch(&key, &self.read_condition(known))? {
            Fetched::Body(reply) => {
                let version = self.version_of(&reply);
                Ok(Some(Versioned { value: Bytes::from(reply.bytes()?), version }))
            }
            Fetched::NotModified | Fetched::Missing => Ok(None),
        }
    }

    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        let key = self.key(path)?;
        if range.start >= range.end {
            return Ok(Bytes::new());
        }
        let headers = vec![("range", format!("bytes={}-{}", range.start, range.end - 1))];
        let reply =
            self.client.execute(Method::GET, &headers, None, &|c, h| presign(self.bucket.get_object(c, &key), h))?;
        match reply.status() {
            200 | 206 => Ok(Bytes::from(reply.bytes()?)),
            416 => Ok(Bytes::new()),
            404 => bail!("{path}: no such object"),
            _ => Err(reply.error("GetObject", &key)),
        }
    }

    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        let key = self.key(path)?;
        let reply = match self.fetch(&key, &Vec::new())? {
            Fetched::Body(reply) => reply,
            Fetched::NotModified | Fetched::Missing => bail!("{path}: no such object"),
        };
        let version = self.version_of(&reply);
        let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut tmp = tempfile::Builder::new().prefix(".download-").tempfile_in(dir)?;
        io::copy(&mut reply.into_reader(), tmp.as_file_mut()).with_context(|| format!("downloading {key}"))?;
        tmp.as_file().sync_all()?;
        tmp.persist(dest).map_err(|e| e.error)?;
        Ok(version)
    }

    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        let key = self.key(path)?;
        self.put_object(&key, &bytes, None)
    }

    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        let key = self.key(path)?;
        let mut file = File::open(src).with_context(|| format!("opening {}", src.display()))?;
        let size = file.metadata()?.len();
        if size <= PART_SIZE as u64 {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            return self.put_object(&key, &bytes, None);
        }
        self.multipart(&key, &mut file)
    }

    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        let key = self.key(path)?;
        self.put_object(&key, &bytes, expected)
    }

    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        let full = format!("{}{prefix}", self.cfg.prefix);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self.list_page(&full, token.as_deref())?;
            out.extend(page.contents.into_iter().filter_map(|c| self.object_meta(c)));
            match page.next_continuation_token {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn delete(&self, path: &str) -> anyhow::Result<()> {
        let key = self.key(path)?;
        let reply = self
            .client
            .execute(Method::DELETE, &Vec::new(), None, &|c, h| presign(self.bucket.delete_object(c, &key), h))?;
        match reply.status() {
            200 | 204 | 404 => Ok(()),
            _ => Err(reply.error("DeleteObject", &key)),
        }
    }

    fn local_path(&self, _path: &str) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        let t = parse_timestamp("2009-10-12T17:50:30.000Z").unwrap();
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_255_369_830);
        assert!(parse_timestamp("yesterday").is_none());
    }

    #[test]
    fn put_failures() {
        let e = || anyhow!("x");
        assert!(matches!(put_failure(412, true, e()), PutError::AlreadyExists));
        assert!(matches!(put_failure(412, false, e()), PutError::Precondition));
        assert!(matches!(put_failure(404, false, e()), PutError::Precondition));
        assert!(matches!(put_failure(403, false, e()), PutError::Other(_)));
    }

    #[test]
    fn gcs_list_namespace() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://doc.s3.amazonaws.com/2006-03-01"><Name>b</Name><Prefix>p/</Prefix><KeyCount>1</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>false</IsTruncated><Contents><Key>p/a</Key><Generation>17</Generation><LastModified>2026-11-17T10:00:00.000Z</LastModified><ETag>"abc"</ETag><Size>3</Size></Contents></ListBucketResult>"#;
        let parsed = parse_list(body).unwrap();
        assert_eq!(parsed.contents.len(), 1);
        assert_eq!(parsed.contents[0].key, "p/a");
        assert!(parse_list("<nope/>").is_err());
    }
}
