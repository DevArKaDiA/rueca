//! Against a real S3-compatible server (MinIO, LocalStack, AWS). Ignored by
//! default; run with
//!
//! ```text
//! RUECA_S3_TEST_URL=s3://rueca-test/ci \
//! AWS_ENDPOINT_URL=http://localhost:9000 AWS_ALLOW_HTTP=true \
//! AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin \
//! cargo test -p rueca-storage --test s3_integration -- --ignored
//! ```
//!
//! The bucket must exist. Every run works under a fresh
//! `<prefix>/run-<unix-nanos>/` and deletes what it wrote.

mod common;

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use common::checks;

#[test]
#[ignore = "needs RUECA_S3_TEST_URL and an S3-compatible server"]
fn s3_round_trip() {
    let base = std::env::var("RUECA_S3_TEST_URL").expect("set RUECA_S3_TEST_URL=s3://bucket/prefix");
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let url = format!("{}/run-{nanos}", base.trim_end_matches('/'));
    let store = rueca_storage::open(&url, &BTreeMap::new()).unwrap();
    let scratch = tempfile::tempdir().unwrap();

    checks::all(store.as_ref(), "t");
    checks::file_round_trip(store.as_ref(), "big", scratch.path());
    checks::cas_counter(store.clone(), "cas", 64);

    for meta in store.list("").unwrap() {
        store.delete(&meta.path).unwrap();
    }
    assert!(store.list("").unwrap().is_empty());
}
