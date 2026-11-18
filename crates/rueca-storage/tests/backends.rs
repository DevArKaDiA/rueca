//! The `Storage` contract on every backend: memory, file, and the S3 client
//! in both flavours against an in-process fake server.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use common::checks;
use common::fake_s3::FakeS3;
use rueca_storage::{Storage, open};

fn options(fake: &FakeS3) -> BTreeMap<String, String> {
    [
        ("endpoint", fake.endpoint.as_str()),
        ("allow_http", "true"),
        ("region", "us-east-1"),
        ("access_key_id", "AKIAIOSFODNN7EXAMPLE"),
        ("aws_secret_access_key", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn file_store(dir: &tempfile::TempDir) -> Arc<dyn Storage> {
    open(&format!("file://{}", dir.path().display()), &BTreeMap::new()).unwrap()
}

#[test]
fn memory_contract() {
    let s = open("memory://contract", &BTreeMap::new()).unwrap();
    checks::all(s.as_ref(), "t");
}

#[test]
fn file_contract() {
    let dir = tempfile::tempdir().unwrap();
    checks::all(file_store(&dir).as_ref(), "t");
}

#[test]
fn s3_contract_on_fake_server() {
    let fake = FakeS3::start();
    let s = open("s3://bucket/store/prefix", &options(&fake)).unwrap();
    checks::all(s.as_ref(), "t");
}

#[test]
fn gcs_contract_on_fake_server() {
    let fake = FakeS3::start();
    let s = open("gs://bucket/store", &options(&fake)).unwrap();
    checks::all(s.as_ref(), "t");
    // Updates compare the generation, kept in `ObjectVersion::version`.
    let v = s.put_new("gen.json", Bytes::from_static(b"1")).unwrap();
    assert!(v.version.is_some());
}

#[test]
fn memory_stores_are_shared_by_name() {
    let a = open("memory://shared-by-name", &BTreeMap::new()).unwrap();
    let b = open("memory://shared-by-name", &BTreeMap::new()).unwrap();
    let other = open("memory://another-name", &BTreeMap::new()).unwrap();
    a.put_new("x", Bytes::from_static(b"1")).unwrap();
    assert_eq!(&b.get("x").unwrap().unwrap().value[..], b"1");
    assert!(other.get("x").unwrap().is_none());
}

#[test]
fn cas_counter_memory() {
    let s = open("memory://cas-counter", &BTreeMap::new()).unwrap();
    let conflicts = checks::cas_counter(s, "t", 64);
    assert!(conflicts > 0, "8 writers never conflicted");
}

#[test]
fn cas_counter_file() {
    let dir = tempfile::tempdir().unwrap();
    let conflicts = checks::cas_counter(file_store(&dir), "t", 64);
    assert!(conflicts > 0, "8 writers never conflicted");
}

#[test]
fn cas_counter_s3_fake() {
    let fake = FakeS3::start();
    let s = open("s3://bucket/p", &options(&fake)).unwrap();
    checks::cas_counter(s, "t", 64);
}

#[test]
fn big_file_memory() {
    let scratch = tempfile::tempdir().unwrap();
    let s = open("memory://big-file", &BTreeMap::new()).unwrap();
    checks::file_round_trip(s.as_ref(), "t", scratch.path());
}

#[test]
fn big_file_file() {
    let (dir, scratch) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let s = file_store(&dir);
    checks::file_round_trip(s.as_ref(), "t", scratch.path());
    assert!(s.local_path("t/segments/big.rcseg").unwrap().is_file());
}

#[test]
fn big_file_s3_multipart() {
    let fake = FakeS3::start();
    let scratch = tempfile::tempdir().unwrap();
    let s = open("s3://bucket/p", &options(&fake)).unwrap();
    checks::file_round_trip(s.as_ref(), "t", scratch.path());
    // One multipart upload completed; the second (AlreadyExists) was aborted.
    assert_eq!(fake.completed_uploads(), 1);
    assert_eq!(fake.pending_uploads(), 0);
}

#[test]
fn big_file_gcs_multipart() {
    let fake = FakeS3::start();
    let scratch = tempfile::tempdir().unwrap();
    let s = open("gs://bucket/p", &options(&fake)).unwrap();
    checks::file_round_trip(s.as_ref(), "t", scratch.path());
    assert_eq!(fake.completed_uploads(), 1);
}
