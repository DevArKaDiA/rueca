//! Read-modify-write of a JSON object by compare-and-swap.

use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{ObjectVersion, PutError, Storage};

/// What a mutation decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CasStep<T> {
    /// Write this value (conditional on the version read).
    Write(T),
    /// Stop without writing; this is the result (e.g. an idempotent entry is
    /// already present).
    Unchanged(T),
}

/// The result of [`cas_update`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CasOutcome<T> {
    /// The value written, or returned by [`CasStep::Unchanged`].
    pub value: T,
    /// The object's version after the write, or the version read when
    /// nothing was written (default when the object does not exist).
    pub version: ObjectVersion,
    /// Whether this call wrote the object.
    pub written: bool,
    /// Writes lost to another writer before this outcome.
    pub conflicts: u32,
}

/// Every attempt of a [`cas_update`] lost to another writer.
#[derive(Debug, thiserror::Error)]
#[error("{path}: compare-and-swap lost to concurrent writers {conflicts} times in {attempts} attempts")]
pub struct CasExhausted {
    pub path: String,
    pub attempts: u32,
    pub conflicts: u32,
}

const BACKOFF_BASE: Duration = Duration::from_millis(10);
const BACKOFF_MAX: Duration = Duration::from_secs(1);

/// Reads the JSON object at `path` (`None` when absent), applies `mutate`,
/// and writes the result conditionally on the version read
/// ([`Storage::put_if`]). When another writer got there first
/// ([`PutError::Precondition`] or [`PutError::AlreadyExists`]) it waits
/// 10 ms × 2ⁿ (at most 1 s, jittered), re-reads and re-applies `mutate` to
/// the fresh value, up to `attempts` times; then fails with
/// [`CasExhausted`]. Errors from `mutate`, decoding or the backend stop at
/// once.
pub fn cas_update<T, F>(
    storage: &dyn Storage,
    path: &str,
    attempts: u32,
    mut mutate: F,
) -> anyhow::Result<CasOutcome<T>>
where
    T: Serialize + DeserializeOwned,
    F: FnMut(Option<T>) -> anyhow::Result<CasStep<T>>,
{
    let attempts = attempts.max(1);
    let mut conflicts = 0;
    for attempt in 0..attempts {
        if attempt > 0 {
            std::thread::sleep(backoff(attempt - 1));
        }
        let (current, read_version) = read_json::<T>(storage, path)?;
        let value = match mutate(current)? {
            CasStep::Unchanged(value) => {
                let version = read_version.unwrap_or_default();
                return Ok(CasOutcome { value, version, written: false, conflicts });
            }
            CasStep::Write(value) => value,
        };
        let bytes = Bytes::from(serde_json::to_vec_pretty(&value)?);
        match storage.put_if(path, bytes, read_version.as_ref()) {
            Ok(version) => return Ok(CasOutcome { value, version, written: true, conflicts }),
            Err(PutError::Precondition | PutError::AlreadyExists) => {
                conflicts += 1;
                tracing::debug!(path, attempt, "compare-and-swap conflict, retrying");
            }
            Err(PutError::Other(e)) => return Err(e),
        }
    }
    Err(CasExhausted { path: path.to_string(), attempts, conflicts }.into())
}

fn read_json<T: DeserializeOwned>(
    storage: &dyn Storage,
    path: &str,
) -> anyhow::Result<(Option<T>, Option<ObjectVersion>)> {
    match storage.get(path)? {
        None => Ok((None, None)),
        Some(v) => {
            let value = serde_json::from_slice(&v.value).map_err(|e| anyhow::anyhow!("{path}: invalid JSON: {e}"))?;
            Ok((Some(value), Some(v.version)))
        }
    }
}

/// `10 ms × 2^n`, capped at 1 s, uniformly jittered over its upper half.
fn backoff(n: u32) -> Duration {
    let full = BACKOFF_BASE.saturating_mul(1u32 << n.min(16)).min(BACKOFF_MAX);
    let ms = u64::try_from(full.as_millis()).unwrap_or(1000).max(1);
    Duration::from_millis(fastrand::u64(ms / 2..=ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        for n in 0..20 {
            let d = backoff(n);
            assert!(d <= BACKOFF_MAX, "{n}: {d:?}");
        }
        assert!(backoff(0) <= Duration::from_millis(10));
        assert!(backoff(3) >= Duration::from_millis(40));
        assert!(backoff(12) >= Duration::from_millis(500));
    }
}
