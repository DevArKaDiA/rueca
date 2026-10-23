//! `write_segment`: content packs first, then the bundle.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use roaring::RoaringBitmap;
use telar_model::{ResolvedGraph, SnapshotInfo, git_blob_sha};

use crate::bundle::BundleWriter;
use crate::packs::{BlobIndex, BlobLoc, PackReader, PackWriter, Sha, compress, parse_sha};
use crate::{ContentSource, FORMAT_VERSION, Result, SegmentError, SegmentMeta, graph, ngram, store, symbols};

fn slug(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(max).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "x".to_string() } else { out }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `<repo-slug>-<commit[..12]>-<unix-ts>-<rand4>`.
pub(crate) fn segment_id(snap: &SnapshotInfo) -> String {
    let commit = slug(&snap.commit, 12);
    format!("{}-{}-{}-{:04x}", slug(&snap.repo, 48), commit, now_secs(), fastrand::u16(..))
}

struct Job {
    sha: Sha,
    rep: usize,
    files: Vec<u32>,
    known: bool,
}

#[derive(Default)]
struct WorkerOut {
    postings: HashMap<u32, RoaringBitmap>,
    text_files: RoaringBitmap,
    new_blobs: Vec<(Sha, u64, u64, u64)>,
}

pub(crate) fn write_segment(graph: &ResolvedGraph, contents: &dyn ContentSource, store_dir: &Path) -> Result<SegmentMeta> {
    graph.validate().map_err(|e| SegmentError::Other(format!("invalid graph: {e}")))?;
    store::ensure_dirs(store_dir)?;
    let tmp_dir = store_dir.join("tmp");
    store::clean_stale_tmp(&tmp_dir);
    let packs_dir = store_dir.join("packs");
    let id = segment_id(&graph.snapshot);

    // ---- content packs -------------------------------------------------
    let (index, index_rebuilt) = BlobIndex::load(&packs_dir)?;
    let mut by_sha: HashMap<Sha, usize> = HashMap::new();
    let mut jobs: Vec<Job> = Vec::new();
    for (i, f) in graph.files.iter().enumerate() {
        let sha = parse_sha(&f.blob_sha)
            .ok_or_else(|| SegmentError::Other(format!("file {}: invalid blob sha {:?}", f.path, f.blob_sha)))?;
        match by_sha.get(&sha) {
            Some(&j) => jobs[j].files.push(i as u32),
            None => {
                by_sha.insert(sha, jobs.len());
                jobs.push(Job { sha, rep: i, files: vec![i as u32], known: index.map.contains_key(&sha) });
            }
        }
    }
    let reader = PackReader::with_index(store_dir, index);
    let pack_id = format!("{}-{:08x}", now_secs(), fastrand::u32(..));
    let pack: Mutex<Option<PackWriter>> = Mutex::new(None);
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(jobs.len().max(1)).min(16);

    let work = |out: &mut WorkerOut| -> Result<()> {
        loop {
            if failed.load(Ordering::Relaxed) {
                return Ok(());
            }
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(job) = jobs.get(i) else { return Ok(()) };
            let file = &graph.files[job.rep];
            // A known blob is read back from its pack; if that fails (pack
            // deleted or damaged) it is fetched and stored again, and the new
            // location supersedes the old one in the blob index.
            let stored = if job.known { reader.read(&file.blob_sha).ok() } else { None };
            let content = match stored {
                Some(c) => c,
                None => {
                    let c = contents.content(file)?;
                    let actual = git_blob_sha(&c);
                    if !actual.eq_ignore_ascii_case(&file.blob_sha) {
                        return Err(SegmentError::Other(format!(
                            "blob sha mismatch for {}: expected {}, content hashes to {actual}",
                            file.path, file.blob_sha
                        )));
                    }
                    let frame = compress(&c)?;
                    let mut guard = pack.lock().unwrap_or_else(|p| p.into_inner());
                    if guard.is_none() {
                        *guard = Some(PackWriter::create(&tmp_dir, &pack_id)?);
                    }
                    let w = guard.as_mut().expect("pack writer just created");
                    let off = w.append(job.sha, &frame, c.len() as u64)?;
                    drop(guard);
                    out.new_blobs.push((job.sha, off, frame.len() as u64, c.len() as u64));
                    c
                }
            };
            if ngram::is_text(&content) {
                let tris = ngram::trigrams(&content);
                for &f in &job.files {
                    out.text_files.insert(f);
                    for &t in &tris {
                        out.postings.entry(t).or_default().insert(f);
                    }
                }
            }
        }
    };

    let results: Vec<Result<WorkerOut>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = WorkerOut::default();
                    let r = work(&mut out);
                    if r.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    r.map(|_| out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(SegmentError::Other("content worker panicked".into()))))
            .collect()
    });
    let pack_writer = pack.into_inner().unwrap_or_else(|p| p.into_inner());
    let mut outs = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(o) => outs.push(o),
            Err(e) => {
                if let Some(w) = pack_writer {
                    w.abort();
                }
                return Err(e);
            }
        }
    }

    let mut index = reader.into_index().unwrap_or_default();
    let mut content_bytes_added = 0u64;
    let new_pack = match pack_writer {
        Some(w) if !w.is_empty() => {
            content_bytes_added = w.finish(&packs_dir, &pack_id)?;
            true
        }
        Some(w) => {
            w.abort();
            false
        }
        None => false,
    };
    if new_pack {
        let p = index.packs.len() as u32;
        index.packs.push(pack_id);
        for o in &outs {
            for &(sha, offset, clen, raw_len) in &o.new_blobs {
                index.map.insert(sha, BlobLoc { pack: p, offset, clen, raw_len });
            }
        }
    }
    if new_pack || index_rebuilt {
        index.save(&packs_dir, &tmp_dir)?;
    }

    // Merge per-worker trigram postings.
    let mut postings: HashMap<u32, RoaringBitmap> = HashMap::new();
    let mut text_files = RoaringBitmap::new();
    for o in outs {
        text_files |= o.text_files;
        for (k, bm) in o.postings {
            match postings.get_mut(&k) {
                Some(acc) => *acc |= bm,
                None => {
                    postings.insert(k, bm);
                }
            }
        }
    }

    // ---- bundle ------------------------------------------------------------
    let mut meta = SegmentMeta {
        id: id.clone(),
        format_version: FORMAT_VERSION,
        snapshot: graph.snapshot.clone(),
        node_count: graph.nodes.len() as u32,
        edge_count: graph.edges.len() as u32,
        file_count: graph.files.len() as u32,
        bytes: 0,
        content_bytes_added,
    };
    let tmp_path = tmp_dir.join(format!("{id}.tlseg.tmp"));
    let build = || -> Result<u64> {
        let mut w = BundleWriter::create(&tmp_path, FORMAT_VERSION)?;
        w.add("meta", &serde_json::to_vec(&meta).map_err(|e| SegmentError::Other(e.to_string()))?)?;
        graph::write_graph(&mut w, graph)?;
        let scratch = tempfile::Builder::new().prefix("tantivy-").tempdir_in(&tmp_dir)?;
        symbols::write_symbols(&mut w, &graph.nodes, &graph.files, scratch.path())?;
        drop(scratch);
        let (keys, offsets, post) = ngram::serialize_postings(postings)?;
        w.add_pod("ngrams.keys", &keys)?;
        w.add_pod("ngrams.offsets", &offsets)?;
        w.add("ngrams.postings", &post)?;
        let mut tf = Vec::new();
        text_files.serialize_into(&mut tf)?;
        w.add("ngrams.text_files", &tf)?;
        w.finish()
    };
    match build() {
        Ok(bytes) => meta.bytes = bytes,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    }
    let segments_dir = store_dir.join("segments");
    if let Err(e) = std::fs::rename(&tmp_path, segments_dir.join(format!("{id}.tlseg"))) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    store::sync_dir(&segments_dir);
    Ok(meta)
}
