//! The storage seam of Rueca's service mode (RFC 0008 §5): one synchronous
//! [`Storage`] trait over object storage with conditional writes, and
//! [`cas_update`], the read-modify-write loop every mutable object goes
//! through.
//!
//! # The trait
//!
//! A store is a flat namespace of `/`-separated object paths
//! (`repos/<repo-dir>/catalog.json`, `orgs/<name>/org.json`, …; RFC 0008
//! §5.1). Every write is conditional:
//!
//! - [`Storage::put_new`] / [`Storage::put_new_file`] create an object and fail
//!   with [`PutError::AlreadyExists`] when it exists (immutable objects:
//!   segments, packs, generations, organization index files).
//! - [`Storage::put_if`] replaces an object only if it still has the
//!   [`ObjectVersion`] the caller read, else [`PutError::Precondition`]
//!   (mutable objects: `store.json`, catalogs, `org.json`). With `None` it is
//!   a create.
//! - [`Storage::get_if_changed`] is a conditional read: `None` when the object
//!   still has the known version (an HTTP `304`) or is gone.
//!
//! The API is synchronous: Rueca's local code is synchronous and the HTTP
//! client is blocking, so no async runtime is involved.
//!
//! # Compare-and-swap ([`cas_update`])
//!
//! A mutation is a function re-applied to the fresh value at every retry, so
//! a retry never overwrites another writer's change (RFC 0008 §5.2):
//!
//! | Class | Objects | Write | On conflict |
//! |---|---|---|---|
//! | Immutable | segments, packs, generations, `index/`, `profiles/`, `links/` | [`Storage::put_new`] (`Create`) | [`PutError::AlreadyExists`]: content-addressed objects ignore it; a generation means another publisher won |
//! | Mutable | `store.json`, catalogs, `org.json` | read, then [`Storage::put_if`] with the read version (`Update`); `Create` for the first write | [`PutError::Precondition`] / [`PutError::AlreadyExists`]: re-read, re-apply, retry with jittered backoff (10 ms × 2ⁿ, at most 1 s), `attempts` times (callers use 8) |
//!
//! [`CasStep::Unchanged`] stops without writing (an idempotent entry already
//! present); [`CasOutcome::conflicts`] counts lost races. Running out of
//! attempts is a [`CasExhausted`] error.
//!
//! # Backends ([`open`])
//!
//! | URL | Backend | `Create` | `Update` |
//! |---|---|---|---|
//! | `memory://<name>` | [`MemoryStorage`]: process-wide, shared by name | under a mutex | e-tag compare under a mutex |
//! | `file:///abs/dir` | [`FileStorage`]: Rueca's own, over `std::fs` | exists check + rename under `<dir>/.lock` | BLAKE3 compare + rename under `<dir>/.lock` |
//! | `s3://bucket/prefix` | [`S3Storage`], S3 flavour | `If-None-Match: *` (412 → `AlreadyExists`) | `If-Match: <e-tag>` (412 or 404 → `Precondition`) |
//! | `gs://bucket/prefix` | [`S3Storage`], GCS XML API flavour, HMAC keys | `x-goog-if-generation-match: 0` | `x-goog-if-generation-match: <generation>` |
//! | `az://…` | none: deferred | | |
//!
//! **`memory://`** keeps objects in a `Mutex<BTreeMap>`; every
//! [`open`] of the same name in one process returns the same store, so tests
//! can run indexer threads and a replica against one `memory://test`. E-tags
//! are a per-store write counter.
//!
//! **`file://`** is single-host by design (RFC 0008 §5.5). Objects are plain
//! files under the directory ([`Storage::local_path`] returns them, so a
//! reader can `mmap` in place). A write goes to a temporary file under
//! `<dir>/.tmp/`, is fsynced, then renamed into place while holding an
//! exclusive advisory lock on `<dir>/.lock` (`std::fs::File::lock`; the same
//! lock file a local store uses), so the existence or version check and the
//! rename are atomic with respect to every other writer, in this process or
//! another. A writer waiting more than 60 s fails naming the lock. Readers
//! take no lock: renames are atomic. The e-tag is the BLAKE3 hex digest of
//! the content, computed on read and on write (no sidecar file can go
//! stale); [`Storage::list`] does not hash and returns no e-tag. Paths whose
//! first component is `.lock` or `.tmp` are reserved.
//!
//! **`s3://` and `gs://`** share one client: requests are built and signed
//! (AWS Signature Version 4, presigned URLs whose signed headers include the
//! conditional headers) by [`rusty_s3`], a sans-IO S3 library, and sent with
//! [`ureq`] over rustls (`ring` provider) trusting the platform's root
//! certificates (`rustls-native-certs`). Operations: `PutObject`,
//! `GetObject` (conditional, ranged), `DeleteObject` (404 is success),
//! `ListObjectsV2` with continuation tokens, and multipart upload
//! (`CreateMultipartUpload`, 8 MiB `UploadPart`s, `CompleteMultipartUpload`
//! carrying the create condition; aborted on failure) for files above 8 MiB.
//! Transport errors, 5xx, 429 and 409 (a concurrent conditional request) are
//! retried three times. GCS is reached through its S3-compatible XML API
//! (`https://storage.googleapis.com`, HMAC interoperability keys, region
//! `auto`); its generation number is kept in [`ObjectVersion::version`] and
//! is what `Update` compares. The GCS flavour is exercised against an
//! in-process fake server only; S3 against the same fake and, through the
//! ignored `s3_round_trip` test, MinIO or LocalStack.
//!
//! Configuration, options map first, then the environment: see
//! [`S3Storage::open`]. Credentials are static (options or environment);
//! the instance metadata service, IRSA and pod identity come in phase 2
//! behind [`CredentialsProvider`]. Without credentials requests are sent
//! unsigned (public buckets, local test servers).
//!
//! # Why not `object_store`
//!
//! RFC 0008 §5.10 chose `object_store` 0.14. Its dependency tree fails
//! `cargo deny check`: `chrono` (RUSTSEC-2026-0543) and `humantime`
//! (RUSTSEC-2026-0542) are unmaintained, both unconditional dependencies of
//! every feature set, and its `reqwest`/rustls stack pulls
//! `webpki-root-certs` (CDLA-Permissive-2.0, not in the license allowlist).
//! This crate needs six operations and conditional headers, which
//! `rusty-s3` + `ureq` cover without either.
//!
//! # Deferred
//!
//! - Azure Blob Storage (`az://`): [`open`] fails with a clear error.
//! - Credentials from IMDS, IRSA (web identity) or pod identity, and
//!   credential refresh (phase 2, [`CredentialsProvider`]).
//! - Hotcache range reads (RFC 0008 §5.6, phase 4): [`Storage::get_range`]
//!   exists so that a range-read bundle has its primitive.

mod cas;
mod file;
mod memory;
mod s3;

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, bail};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

pub use cas::{CasExhausted, CasOutcome, CasStep, cas_update};
pub use file::FileStorage;
pub use memory::MemoryStorage;
pub use s3::{Credentials, CredentialsProvider, Flavour, S3Storage, StaticCredentials};

/// The version of an object as the backend reports it: the e-tag (S3,
/// Azure, memory, file) and, where the backend has one, a version or
/// generation number (GCS). Both are kept because GCS compares generations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectVersion {
    pub e_tag: Option<String>,
    pub version: Option<String>,
}

/// A value read together with the version it was read at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Versioned<T> {
    pub value: T,
    pub version: ObjectVersion,
}

/// One object returned by [`Storage::list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    /// Object path, relative to the store (no store prefix).
    pub path: String,
    pub size: u64,
    pub version: ObjectVersion,
    pub last_modified: Option<SystemTime>,
}

/// Why a conditional write did not happen.
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// `Create` found an object at the path.
    #[error("object already exists")]
    AlreadyExists,
    /// `Update` found another version (or no object).
    #[error("version precondition failed")]
    Precondition,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<std::io::Error> for PutError {
    fn from(e: std::io::Error) -> Self {
        PutError::Other(e.into())
    }
}

/// Object storage with conditional writes. Paths are `/`-separated, relative,
/// without empty, `.` or `..` components.
pub trait Storage: Send + Sync + 'static {
    /// Reads an object; `None` if it does not exist.
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>>;

    /// Reads an object unless it still has version `known`; `None` when it is
    /// unchanged or does not exist (use [`Storage::get`] to tell them apart).
    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>>;

    /// Reads bytes `range` of an object (`end` is clamped to its size); an
    /// error if it does not exist.
    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes>;

    /// Downloads an object into `dest` through a temporary file in `dest`'s
    /// directory renamed into place; an error if it does not exist.
    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion>;

    /// Creates an object; [`PutError::AlreadyExists`] if one exists.
    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError>;

    /// Creates an object from a local file, with [`Storage::put_new`]'s
    /// semantics (multipart above 8 MiB on object stores).
    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError>;

    /// `expected = None`: create, as [`Storage::put_new`]. `Some(v)`: replace
    /// the object only if its version is still `v`, else
    /// [`PutError::Precondition`].
    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError>;

    /// Every object whose path starts with `prefix` (a string prefix, not a
    /// directory), recursively, sorted by path.
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>>;

    /// Deletes an object; a missing object is not an error.
    fn delete(&self, path: &str) -> anyhow::Result<()>;

    /// The file holding the object, for filesystem backends (so a reader can
    /// `mmap` it in place); `None` for object stores.
    fn local_path(&self, path: &str) -> Option<PathBuf>;
}

/// Opens a store by URL. `options` are backend configuration keys (see
/// [`S3Storage::open`]); `memory://` and `file://` take none.
///
/// - `memory://<name>`: [`MemoryStorage::named`]
/// - `file:///abs/dir`: [`FileStorage::open`]
/// - `s3://bucket[/prefix]`, `gs://bucket[/prefix]`: [`S3Storage::open`]
pub fn open(url: &str, options: &BTreeMap<String, String>) -> anyhow::Result<Arc<dyn Storage>> {
    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("store URL `{url}` has no scheme (memory://, file://, s3://, gs://)"))?;
    let no_options = || -> anyhow::Result<()> {
        if let Some(key) = options.keys().next() {
            bail!("store option `{key}` does not apply to {scheme}:// stores");
        }
        Ok(())
    };
    match scheme {
        "memory" => {
            no_options()?;
            Ok(MemoryStorage::named(rest))
        }
        "file" => {
            no_options()?;
            Ok(Arc::new(FileStorage::open(Path::new(&file_url_path(rest)?))?))
        }
        "s3" => Ok(Arc::new(S3Storage::open(Flavour::S3, rest, options)?)),
        "gs" => Ok(Arc::new(S3Storage::open(Flavour::Gcs, rest, options)?)),
        "az" | "azure" | "abfs" | "abfss" => bail!(
            "store URL `{url}`: Azure Blob Storage is not supported in this phase (RFC 0008 phase 0 supports memory://, file://, s3:// and gs://)"
        ),
        other => bail!("store URL `{url}`: unknown scheme `{other}` (memory://, file://, s3://, gs://)"),
    }
}

/// `file://` URL remainder → filesystem path. Only absolute paths
/// (`file:///abs`); no host, no percent-decoding.
fn file_url_path(rest: &str) -> anyhow::Result<String> {
    let Some(path) = rest.strip_prefix('/') else {
        bail!("file:// store URL needs an absolute path (file:///abs/dir), got `file://{rest}`");
    };
    // `file:///C:/dir` on Windows.
    if path.as_bytes().get(1) == Some(&b':') {
        return Ok(path.to_string());
    }
    Ok(format!("/{path}"))
}

/// Rejects paths that are empty, absolute, have empty / `.` / `..`
/// components, backslashes or NUL bytes.
pub(crate) fn check_path(path: &str) -> anyhow::Result<()> {
    let bad = path.is_empty()
        || path.contains(['\\', '\0'])
        || path.split('/').any(|c| c.is_empty() || c == "." || c == "..");
    if bad {
        bail!("invalid object path `{path}`");
    }
    Ok(())
}

/// Lowercase hex of a BLAKE3 digest.
pub(crate) fn blake3_hex(hash: &blake3::Hash) -> String {
    hash.to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        for ok in ["a", "a/b.json", "repos/x-1/segments/s.rcseg"] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/a", "a/", "a//b", "./a", "a/../b", "a\\b", "a\0"] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url_path("/tmp/x").unwrap(), "/tmp/x");
        assert_eq!(file_url_path("/C:/x").unwrap(), "C:/x");
        assert!(file_url_path("relative/x").is_err());
    }

    #[test]
    fn unsupported_schemes() {
        let none = BTreeMap::new();
        let err = open("az://container/p", &none).err().unwrap().to_string();
        assert!(err.contains("not supported in this phase"), "{err}");
        assert!(open("ftp://x", &none).is_err());
        assert!(open("no-scheme", &none).is_err());
        let mut opts = BTreeMap::new();
        opts.insert("region".to_string(), "x".to_string());
        assert!(open("memory://opts", &opts).is_err());
    }
}
