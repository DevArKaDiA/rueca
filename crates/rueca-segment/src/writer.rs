//! `write_segment`: content packs first, then the bundle.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use roaring::RoaringBitmap;
use rueca_model::{ResolvedGraph, SnapshotInfo, git_blob_sha};

use crate::bundle::BundleWriter;
use crate::packs::{BlobIndex, BlobLoc, PackReader, PackWriter, Sha, compress, parse_sha};
use crate::{ContentSource, FORMAT_VERSION, Result, SegmentError, SegmentMeta, graph, ngram, priors, store, symbols};

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
    /// `(node, body text)` for the symbol index.
    bodies: Vec<(u32, String)>,
}

pub(crate) fn write_segment(
    graph: &ResolvedGraph,
    contents: &dyn ContentSource,
    store_dir: &Path,
) -> Result<SegmentMeta> {
    graph.validate().map_err(|e| SegmentError::Other(format!("invalid graph: {e}")))?;
    store::ensure_dirs(store_dir)?;
    let tmp_dir = store_dir.join("tmp");
    store::clean_stale_tmp(&tmp_dir);
    let packs_dir = store_dir.join("packs");
    let id = segment_id(&graph.snapshot);

    // ---- content packs -------------------------------------------------
    let (index, index_rebuilt) = BlobIndex::load(&packs_dir)?;
    let jobs = blob_jobs(graph, &index)?;
    let work = ContentWork {
        graph,
        contents,
        file_nodes: file_symbol_nodes(graph),
        reader: PackReader::with_index(store_dir, index),
        tmp_dir: &tmp_dir,
        pack_id: format!("{}-{:08x}", now_secs(), fastrand::u32(..)),
        pack: Mutex::new(None),
        next: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
        jobs,
    };
    let results = work.run_workers();
    let ContentWork { reader, pack, pack_id, .. } = work;
    let pack_writer = pack.into_inner().unwrap_or_else(|p| p.into_inner());
    let outs = match results.into_iter().collect::<Result<Vec<WorkerOut>>>() {
        Ok(outs) => outs,
        Err(e) => {
            if let Some(w) = pack_writer {
                w.abort();
            }
            return Err(e);
        }
    };
    let mut index = reader.into_index().unwrap_or_default();
    let (new_pack, content_bytes_added) = finish_pack(pack_writer, &packs_dir, &pack_id)?;
    if new_pack {
        let p = index.packs.len() as u32;
        index.packs.push(pack_id);
        for &(sha, offset, clen, raw_len) in outs.iter().flat_map(|o| &o.new_blobs) {
            index.map.insert(sha, BlobLoc { pack: p, offset, clen, raw_len });
        }
    }
    if new_pack || index_rebuilt {
        index.save(&packs_dir, &tmp_dir)?;
    }
    let merged = Merged::from_outputs(outs, graph);

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
    let tmp_path = tmp_dir.join(format!("{id}.rcseg.tmp"));
    match write_bundle(&tmp_path, &tmp_dir, &meta, graph, merged) {
        Ok(bytes) => meta.bytes = bytes,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    }
    let segments_dir = store_dir.join("segments");
    if let Err(e) = std::fs::rename(&tmp_path, segments_dir.join(format!("{id}.rcseg"))) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    store::sync_dir(&segments_dir);
    Ok(meta)
}

/// One job per distinct blob of the snapshot, with the files that have it.
fn blob_jobs(graph: &ResolvedGraph, index: &BlobIndex) -> Result<Vec<Job>> {
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
    Ok(jobs)
}

/// Symbol nodes (with a span) per file, for the symbol index `body` field.
fn file_symbol_nodes(graph: &ResolvedGraph) -> Vec<Vec<u32>> {
    let mut file_nodes: Vec<Vec<u32>> = vec![Vec::new(); graph.files.len()];
    for (i, n) in graph.nodes.iter().enumerate() {
        if let Some(f) = n.file
            && n.span.is_some()
            && let Some(v) = file_nodes.get_mut(f as usize)
        {
            v.push(i as u32);
        }
    }
    file_nodes
}

/// The content workers' shared state: they take jobs in turn, store new
/// blobs in one pack and index the text.
struct ContentWork<'a> {
    graph: &'a ResolvedGraph,
    contents: &'a dyn ContentSource,
    jobs: Vec<Job>,
    file_nodes: Vec<Vec<u32>>,
    reader: PackReader,
    tmp_dir: &'a Path,
    pack_id: String,
    pack: Mutex<Option<PackWriter>>,
    next: AtomicUsize,
    failed: AtomicBool,
}

impl ContentWork<'_> {
    /// Runs the workers (at most 16, one per job at least) to completion.
    fn run_workers(&self) -> Vec<Result<WorkerOut>> {
        let threads =
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(self.jobs.len().max(1)).min(16);
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let mut out = WorkerOut::default();
                        let r = self.work(&mut out);
                        if r.is_err() {
                            self.failed.store(true, Ordering::Relaxed);
                        }
                        r.map(|_| out)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(SegmentError::Other("content worker panicked".into()))))
                .collect()
        })
    }

    /// One worker: jobs until none is left or a worker failed.
    fn work(&self, out: &mut WorkerOut) -> Result<()> {
        while !self.failed.load(Ordering::Relaxed) {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            let Some(job) = self.jobs.get(i) else { return Ok(()) };
            let content = self.job_content(job, out)?;
            if ngram::is_text(&content) {
                self.index_text(job, &content, out);
            }
        }
        Ok(())
    }

    /// A known blob is read back from its pack; if that fails (pack deleted
    /// or damaged) it is fetched and stored again, and the new location
    /// supersedes the old one in the blob index.
    fn job_content(&self, job: &Job, out: &mut WorkerOut) -> Result<Vec<u8>> {
        let file = &self.graph.files[job.rep];
        if job.known
            && let Ok(c) = self.reader.read(&file.blob_sha)
        {
            return Ok(c);
        }
        let c = self.contents.content(file)?;
        let actual = git_blob_sha(&c);
        if !actual.eq_ignore_ascii_case(&file.blob_sha) {
            return Err(SegmentError::Other(format!(
                "blob sha mismatch for {}: expected {}, content hashes to {actual}",
                file.path, file.blob_sha
            )));
        }
        let frame = compress(&c)?;
        let mut guard = self.pack.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(PackWriter::create(self.tmp_dir, &self.pack_id)?);
        }
        let w = guard.as_mut().expect("pack writer just created");
        let off = w.append(job.sha, &frame, c.len() as u64)?;
        drop(guard);
        out.new_blobs.push((job.sha, off, frame.len() as u64, c.len() as u64));
        Ok(c)
    }

    /// Symbol bodies and trigram postings of a text blob, for each of its
    /// files.
    fn index_text(&self, job: &Job, content: &[u8], out: &mut WorkerOut) {
        for &f in &job.files {
            out.bodies.extend(symbols::body_texts(&self.file_nodes[f as usize], &self.graph.nodes, content));
        }
        let tris = ngram::trigrams(content);
        for &f in &job.files {
            out.text_files.insert(f);
            for &t in &tris {
                out.postings.entry(t).or_default().insert(f);
            }
        }
    }
}

/// Moves a non-empty pack into the store (dropping an empty one): whether
/// there is a new pack, and its size.
fn finish_pack(pack_writer: Option<PackWriter>, packs_dir: &Path, pack_id: &str) -> Result<(bool, u64)> {
    match pack_writer {
        Some(w) if !w.is_empty() => Ok((true, w.finish(packs_dir, pack_id)?)),
        Some(w) => {
            w.abort();
            Ok((false, 0))
        }
        None => Ok((false, 0)),
    }
}

/// The workers' outputs merged: trigram postings, text files, symbol
/// bodies, and the C/C++ callables without a body (declaration prior).
struct Merged {
    postings: HashMap<u32, RoaringBitmap>,
    text_files: RoaringBitmap,
    bodies: Vec<Option<String>>,
    bodiless: RoaringBitmap,
}

impl Merged {
    fn from_outputs(outs: Vec<WorkerOut>, graph: &ResolvedGraph) -> Merged {
        let mut postings: HashMap<u32, RoaringBitmap> = HashMap::new();
        let mut text_files = RoaringBitmap::new();
        let mut bodies: Vec<Option<String>> = vec![None; graph.nodes.len()];
        for o in outs {
            for (n, b) in o.bodies {
                bodies[n as usize] = Some(b);
            }
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
        let bodiless: RoaringBitmap = bodies
            .iter()
            .enumerate()
            .filter(|(i, b)| b.as_deref().is_some_and(|b| priors::is_bodiless(&graph.nodes[*i], b)))
            .map(|(i, _)| i as u32)
            .collect();
        Merged { postings, text_files, bodies, bodiless }
    }
}

/// Writes the segment bundle at `tmp_path`; its size.
fn write_bundle(
    tmp_path: &Path,
    tmp_dir: &Path,
    meta: &SegmentMeta,
    graph: &ResolvedGraph,
    merged: Merged,
) -> Result<u64> {
    let mut w = BundleWriter::create(tmp_path, FORMAT_VERSION)?;
    w.add("meta", &serde_json::to_vec(meta).map_err(|e| SegmentError::Other(e.to_string()))?)?;
    graph::write_graph(&mut w, graph)?;
    let scratch = tempfile::Builder::new().prefix("tantivy-").tempdir_in(tmp_dir)?;
    symbols::write_symbols(&mut w, &graph.nodes, &graph.files, &merged.bodies, scratch.path())?;
    drop(scratch);
    let mut bl = Vec::new();
    merged.bodiless.serialize_into(&mut bl)?;
    w.add(symbols::BODILESS_COMPONENT, &bl)?;
    let (keys, offsets, post) = ngram::serialize_postings(merged.postings)?;
    w.add_pod("ngrams.keys", &keys)?;
    w.add_pod("ngrams.offsets", &offsets)?;
    w.add("ngrams.postings", &post)?;
    let mut tf = Vec::new();
    merged.text_files.serialize_into(&mut tf)?;
    w.add("ngrams.text_files", &tf)?;
    w.finish()
}
