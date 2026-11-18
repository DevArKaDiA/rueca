//! Backend-independent checks of the `Storage` contract. Every check works
//! under its own `prefix` so they can share one bucket.

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use rueca_storage::{CasExhausted, CasStep, ObjectVersion, PutError, Storage, cas_update};

pub fn create_fails_on_existing(s: &dyn Storage, prefix: &str) {
    let p = format!("{prefix}/create.json");
    let v = s.put_new(&p, Bytes::from_static(b"one")).unwrap();
    assert!(v.e_tag.is_some());
    assert!(matches!(s.put_new(&p, Bytes::from_static(b"two")), Err(PutError::AlreadyExists)));
    assert!(matches!(s.put_if(&p, Bytes::from_static(b"two"), None), Err(PutError::AlreadyExists)));
    assert_eq!(&s.get(&p).unwrap().unwrap().value[..], b"one");
}

pub fn update_with_stale_version_fails(s: &dyn Storage, prefix: &str) {
    let p = format!("{prefix}/update.json");
    let v1 = s.put_if(&p, Bytes::from_static(b"1"), None).unwrap();
    let v2 = s.put_if(&p, Bytes::from_static(b"2"), Some(&v1)).unwrap();
    assert_ne!(v1, v2);
    let stale = s.put_if(&p, Bytes::from_static(b"3"), Some(&v1));
    assert!(matches!(stale, Err(PutError::Precondition)), "{stale:?}");
    let got = s.get(&p).unwrap().unwrap();
    assert_eq!((&got.value[..], got.version.e_tag.clone()), (&b"2"[..], v2.e_tag.clone()));
    let missing = s.put_if(&format!("{prefix}/absent.json"), Bytes::from_static(b"x"), Some(&v2));
    assert!(matches!(missing, Err(PutError::Precondition)), "{missing:?}");
}

pub fn conditional_get(s: &dyn Storage, prefix: &str) {
    let p = format!("{prefix}/pointer.json");
    assert!(s.get(&p).unwrap().is_none());
    assert!(s.get_if_changed(&p, &ObjectVersion::default()).unwrap().is_none());
    let v1 = s.put_new(&p, Bytes::from_static(b"{\"generation\":1}")).unwrap();
    let read = s.get(&p).unwrap().unwrap();
    assert_eq!(read.version.e_tag, v1.e_tag);
    assert!(s.get_if_changed(&p, &read.version).unwrap().is_none());
    let v2 = s.put_if(&p, Bytes::from_static(b"{\"generation\":2}"), Some(&read.version)).unwrap();
    let changed = s.get_if_changed(&p, &read.version).unwrap().unwrap();
    assert_eq!(&changed.value[..], b"{\"generation\":2}");
    assert_eq!(changed.version.e_tag, v2.e_tag);
}

pub fn list_prefixes(s: &dyn Storage, prefix: &str) {
    let names = ["a/1", "a/2", "a/x/5", "ab/3", "b/4", "b/6", "b/7"];
    for n in names {
        s.put_new(&format!("{prefix}/{n}"), Bytes::from(n.as_bytes().to_vec())).unwrap();
    }
    let paths = |p: &str| -> Vec<String> {
        let full = format!("{prefix}/{p}");
        s.list(&full).unwrap().into_iter().map(|m| m.path[prefix.len() + 1..].to_string()).collect()
    };
    assert_eq!(paths("a/"), ["a/1", "a/2", "a/x/5"]);
    assert_eq!(paths("a"), ["a/1", "a/2", "a/x/5", "ab/3"]);
    assert_eq!(paths(""), names);
    assert!(paths("zz").is_empty());
    let meta = s.list(&format!("{prefix}/a/x/")).unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0].size, 5);
    assert!(meta[0].last_modified.is_some());
}

pub fn delete_and_ranges(s: &dyn Storage, prefix: &str) {
    let p = format!("{prefix}/digits");
    s.put_new(&p, Bytes::from_static(b"0123456789")).unwrap();
    assert_eq!(&s.get_range(&p, 2..5).unwrap()[..], b"234");
    assert_eq!(&s.get_range(&p, 8..100).unwrap()[..], b"89");
    assert!(s.get_range(&p, 20..30).unwrap().is_empty());
    s.delete(&p).unwrap();
    s.delete(&p).unwrap();
    assert!(s.get(&p).unwrap().is_none());
    assert!(s.get_range(&p, 0..1).is_err());
    assert!(s.get("../escape").is_err());
}

/// 8 threads × 50 increments of a JSON counter through `cas_update`.
pub fn cas_counter(s: Arc<dyn Storage>, prefix: &str, attempts: u32) -> u32 {
    let p = format!("{prefix}/counter.json");
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (s, p) = (s.clone(), p.clone());
            std::thread::spawn(move || {
                let mut conflicts = 0;
                for _ in 0..50 {
                    let out =
                        cas_update(s.as_ref(), &p, attempts, |n: Option<u64>| Ok(CasStep::Write(n.unwrap_or(0) + 1)))
                            .unwrap();
                    assert!(out.written);
                    conflicts += out.conflicts;
                }
                conflicts
            })
        })
        .collect();
    let conflicts = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let value: u64 = serde_json::from_slice(&s.get(&p).unwrap().unwrap().value).unwrap();
    assert_eq!(value, 400);
    conflicts
}

pub fn cas_unchanged_and_exhausted(s: &dyn Storage, prefix: &str) {
    let p = format!("{prefix}/idempotent.json");
    let first = cas_update(s, &p, 8, |v: Option<Vec<String>>| {
        let mut v = v.unwrap_or_default();
        if v.contains(&"x".to_string()) {
            return Ok(CasStep::Unchanged(v));
        }
        v.push("x".into());
        Ok(CasStep::Write(v))
    })
    .unwrap();
    assert!(first.written);
    let again = cas_update(s, &p, 8, |v: Option<Vec<String>>| Ok(CasStep::Unchanged(v.unwrap_or_default()))).unwrap();
    assert!(!again.written);
    assert_eq!(again.version.e_tag, first.version.e_tag);
    // Every attempt races a writer that changes the object first (new
    // content: the file backend's e-tag is a content hash).
    let err = cas_update(s, &p, 3, |v: Option<Vec<String>>| {
        let current = s.get(&p)?.unwrap();
        let mut raced: Vec<String> = serde_json::from_slice(&current.value)?;
        raced.push("raced".into());
        let bytes = Bytes::from(serde_json::to_vec(&raced)?);
        s.put_if(&p, bytes, Some(&current.version)).map_err(anyhow::Error::from)?;
        Ok(CasStep::Write(v.unwrap_or_default()))
    })
    .unwrap_err();
    let exhausted = err.downcast_ref::<CasExhausted>().unwrap();
    assert_eq!((exhausted.attempts, exhausted.conflicts), (3, 3));
}

/// Writes 20 MB, uploads it with `put_new_file`, downloads it with
/// `get_to_file` and compares.
pub fn file_round_trip(s: &dyn Storage, prefix: &str, scratch: &Path) {
    let src = scratch.join("src.bin");
    let mut data = vec![0u8; 20 * 1024 * 1024 + 123];
    blake3::Hasher::new().update(prefix.as_bytes()).finalize_xof().fill(&mut data);
    std::fs::write(&src, &data).unwrap();
    let p = format!("{prefix}/segments/big.rcseg");
    let put = s.put_new_file(&p, &src).unwrap();
    assert!(matches!(s.put_new_file(&p, &src), Err(PutError::AlreadyExists)));
    let dest = scratch.join("dest.bin");
    let got = s.get_to_file(&p, &dest).unwrap();
    assert_eq!(got.e_tag, put.e_tag);
    assert!(std::fs::read(&dest).unwrap() == data, "downloaded bytes differ");
    assert_eq!(s.list(&p).unwrap()[0].size, data.len() as u64);
    assert!(s.get_to_file(&format!("{prefix}/absent"), &dest).is_err());
}

/// Every check, each under its own prefix of `prefix`.
pub fn all(s: &dyn Storage, prefix: &str) {
    create_fails_on_existing(s, &format!("{prefix}/create"));
    update_with_stale_version_fails(s, &format!("{prefix}/update"));
    conditional_get(s, &format!("{prefix}/cond"));
    list_prefixes(s, &format!("{prefix}/list"));
    delete_and_ranges(s, &format!("{prefix}/delete"));
    cas_unchanged_and_exhausted(s, &format!("{prefix}/cas"));
}
