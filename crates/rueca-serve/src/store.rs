//! A [`Storage`] decorator counting requests by operation and result
//! (`rueca_store_requests_total{op,result}`).

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use rueca_storage::{ObjectMeta, ObjectVersion, PutError, Storage, Versioned};

use crate::metrics::Metrics;

pub struct CountingStorage {
    inner: Arc<dyn Storage>,
    metrics: Metrics,
}

impl CountingStorage {
    pub fn new(inner: Arc<dyn Storage>, metrics: Metrics) -> CountingStorage {
        CountingStorage { inner, metrics }
    }

    fn count(&self, op: &str, result: &str) {
        self.metrics.counter("rueca_store_requests_total", &[("op", op), ("result", result)]).increment(1);
    }

    fn read<T>(&self, op: &str, r: anyhow::Result<Option<T>>, absent: &str) -> anyhow::Result<Option<T>> {
        self.count(
            op,
            match &r {
                Ok(Some(_)) => "ok",
                Ok(None) => absent,
                Err(_) => "error",
            },
        );
        r
    }

    fn done<T>(&self, op: &str, r: anyhow::Result<T>) -> anyhow::Result<T> {
        self.count(op, if r.is_ok() { "ok" } else { "error" });
        r
    }

    fn put(&self, r: Result<ObjectVersion, PutError>) -> Result<ObjectVersion, PutError> {
        self.count(
            "put",
            match &r {
                Ok(_) => "ok",
                Err(PutError::AlreadyExists) => "exists",
                Err(PutError::Precondition) => "precondition",
                Err(PutError::Other(_)) => "error",
            },
        );
        r
    }
}

impl Storage for CountingStorage {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.read("get", self.inner.get(path), "not_found")
    }
    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.read("get", self.inner.get_if_changed(path, known), "not_modified")
    }
    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        self.done("get_range", self.inner.get_range(path, range))
    }
    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        self.done("get", self.inner.get_to_file(path, dest))
    }
    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        self.put(self.inner.put_new(path, bytes))
    }
    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        self.put(self.inner.put_new_file(path, src))
    }
    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        self.put(self.inner.put_if(path, bytes, expected))
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        self.done("list", self.inner.list(prefix))
    }
    fn delete(&self, path: &str) -> anyhow::Result<()> {
        self.done("delete", self.inner.delete(path))
    }
    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.inner.local_path(path)
    }
}
