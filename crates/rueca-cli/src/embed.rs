//! `rueca embed`: a batch entry point for vectors (RFC 0007 §8.3), used by
//! the bench's vector arm and CoIR runner.
//!
//! Input is JSON lines `{"id": ..., "text": "...", "query": false}`; output
//! is JSON lines `{"id": ..., "vector": [...]}` in input order, or with
//! `--format f32le` raw little-endian `f32` rows plus `<output>.ids` (one id
//! per line).

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use clap::{Args, ValueEnum};
use rueca_embed::{EmbedInput, EmbedOptions, Embedder, InputKind, ModelCache};
use serde::Deserialize;
use serde_json::{Value, json};

/// Records embedded per call: batch order (by length and content) is
/// fixed within a chunk, so the same input file gives the same batches.
const CHUNK: usize = 8192;

/// Output encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// `{"id": ..., "vector": [...]}` per line.
    Jsonl,
    /// Raw little-endian f32 rows; ids in `<output>.ids`.
    F32le,
}

/// Arguments of `rueca embed`.
#[derive(Args, Debug)]
pub struct EmbedArgs {
    /// Model preset (`rueca models list`).
    #[arg(long, env = "RUECA_EMBED_PRESET", default_value = rueca_embed::DEFAULT_PRESET)]
    preset: String,
    /// Output dimensions (Matryoshka truncation, re-normalized).
    #[arg(long, value_name = "N")]
    dims: Option<usize>,
    /// Token budget per input, special tokens included.
    #[arg(long, value_name = "N", default_value_t = rueca_embed::DEFAULT_MAX_TOKENS)]
    max_tokens: usize,
    /// Tokens per batch.
    #[arg(long, value_name = "N", default_value_t = rueca_embed::DEFAULT_BATCH_TOKENS)]
    batch: usize,
    /// Run on the CPU even when a GPU backend is available.
    #[arg(long)]
    cpu: bool,
    /// Worker threads (default: physical cores).
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
    /// Query instruction (default: the preset's); for an encoder preset
    /// (`coderank-137m`) the whole query prefix, used verbatim.
    #[arg(long, value_name = "TEXT")]
    instruction: Option<String>,
    /// JSON lines input.
    #[arg(long, value_name = "FILE|-", default_value = "-")]
    input: String,
    /// Output.
    #[arg(long, value_name = "FILE|-", default_value = "-")]
    output: String,
    /// Output encoding.
    #[arg(long, value_enum, default_value_t = Format::Jsonl)]
    format: Format,
    /// Print the model record (keys, sha256, dims, runtime, backend, CPU level) as JSON,
    /// without loading the model (`load_ms` is null).
    #[arg(long)]
    info: bool,
    /// Embed one string and print its vector as JSON.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["info", "bench"])]
    text: Option<String>,
    /// With --text: embed it as a query.
    #[arg(long, requires = "text")]
    query: bool,
    /// Embed synthetic inputs and print throughput as JSON.
    #[arg(long, conflicts_with = "info")]
    bench: bool,
    /// --bench: number of inputs.
    #[arg(long, value_name = "N", default_value_t = 2000, requires = "bench")]
    n: usize,
    /// --bench: words per synthetic input (about one token each).
    #[arg(long, value_name = "N", default_value_t = 200, requires = "bench")]
    tokens: usize,
    /// --bench: timed runs (the median is reported).
    #[arg(long, value_name = "N", default_value_t = 1, requires = "bench")]
    runs: usize,
}

#[derive(Deserialize)]
struct Record {
    id: Value,
    text: String,
    #[serde(default)]
    query: bool,
}

/// Runs `rueca embed`.
pub fn run(store: &Path, args: &EmbedArgs, pretty: bool) -> anyhow::Result<()> {
    let preset = args.preset.as_str();
    let opts = EmbedOptions {
        dims: args.dims,
        max_tokens: args.max_tokens,
        batch_tokens: args.batch,
        threads: args.threads,
        cpu: args.cpu,
        query_instruction: args.instruction.clone(),
        text_builder_version: rueca_embed::TEXT_BUILDER_VERSION,
    };
    if args.info {
        // The record without loading the model (the bench asks per instance).
        let mut v = serde_json::to_value(rueca_embed::backend::describe_preset(preset, &opts)?)?;
        v["load_ms"] = Value::Null;
        println!("{}", crate::render(&v, pretty));
        return Ok(());
    }
    let cache = ModelCache::for_store(store);
    let started = Instant::now();
    let embedder = rueca_embed::backend::load_preset(&cache, preset, &opts)?;
    let load_ms = started.elapsed().as_secs_f64() * 1e3;
    if let Some(text) = &args.text {
        let kind = if args.query { InputKind::Query } else { InputKind::Document };
        let v = embedder.embed(&[EmbedInput { text: text.clone(), kind }])?;
        let first = v.first().ok_or_else(|| anyhow::anyhow!("no vector returned"))?;
        println!("{}", serde_json::to_string(first)?);
        return Ok(());
    }
    if args.bench {
        let report = bench(embedder.as_ref(), args.n, args.tokens, args.runs, load_ms)?;
        println!("{}", crate::render(&report, pretty));
        return Ok(());
    }
    embed_stream(embedder.as_ref(), args)
}

fn embed_stream(embedder: &dyn Embedder, args: &EmbedArgs) -> anyhow::Result<()> {
    let input: Box<dyn BufRead> = match args.input.as_str() {
        "-" => Box::new(BufReader::new(std::io::stdin().lock())),
        path => Box::new(BufReader::new(File::open(path).map_err(|e| anyhow::anyhow!("cannot open `{path}`: {e}"))?)),
    };
    let mut sink = Sink::open(&args.output, args.format)?;
    let mut chunk = Vec::with_capacity(CHUNK);
    for (n, line) in input.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record =
            serde_json::from_str(&line).map_err(|e| anyhow::anyhow!("input line {}: not a record: {e}", n + 1))?;
        chunk.push(rec);
        if chunk.len() == CHUNK {
            flush(embedder, &mut chunk, &mut sink)?;
        }
    }
    flush(embedder, &mut chunk, &mut sink)?;
    sink.finish()
}

fn flush(embedder: &dyn Embedder, chunk: &mut Vec<Record>, sink: &mut Sink) -> anyhow::Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    let inputs: Vec<EmbedInput> = chunk
        .iter()
        .map(|r| EmbedInput {
            text: r.text.clone(),
            kind: if r.query { InputKind::Query } else { InputKind::Document },
        })
        .collect();
    let vectors = embedder.embed(&inputs)?;
    for (rec, v) in chunk.iter().zip(&vectors) {
        sink.write(&rec.id, v)?;
    }
    chunk.clear();
    Ok(())
}

/// Where vectors go.
struct Sink {
    out: Box<dyn Write>,
    ids: Option<BufWriter<File>>,
    format: Format,
}

impl Sink {
    fn open(output: &str, format: Format) -> anyhow::Result<Self> {
        let create = |p: &str| File::create(p).map_err(|e| anyhow::anyhow!("cannot create `{p}`: {e}"));
        let out: Box<dyn Write> = match output {
            "-" if format == Format::F32le => anyhow::bail!("--format f32le needs --output FILE (ids go to FILE.ids)"),
            "-" => Box::new(BufWriter::new(std::io::stdout().lock())),
            path => Box::new(BufWriter::new(create(path)?)),
        };
        let ids = match format {
            Format::F32le => Some(BufWriter::new(create(&format!("{output}.ids"))?)),
            Format::Jsonl => None,
        };
        Ok(Self { out, ids, format })
    }

    fn write(&mut self, id: &Value, v: &[f32]) -> anyhow::Result<()> {
        match (self.format, self.ids.as_mut()) {
            (Format::F32le, Some(ids)) => {
                for x in v {
                    self.out.write_all(&x.to_le_bytes())?;
                }
                writeln!(ids, "{}", id_line(id))?;
            }
            _ => writeln!(self.out, "{}", json!({ "id": id, "vector": v }))?,
        }
        Ok(())
    }

    fn finish(mut self) -> anyhow::Result<()> {
        self.out.flush()?;
        if let Some(ids) = self.ids.as_mut() {
            ids.flush()?;
        }
        Ok(())
    }
}

/// An id as one line: strings bare, anything else as JSON.
fn id_line(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Words for synthetic inputs: common English and code words, most of them
/// one token for byte-pair tokenizers.
const WORDS: &[&str] = &[
    "the", "function", "returns", "a", "value", "for", "each", "request", "user", "session", "token", "file", "index",
    "query", "result", "error", "list", "map", "key", "string", "number", "time", "data", "read", "write", "open",
    "close", "parse", "build", "check", "cache", "store", "path", "name", "type", "class", "method", "call", "graph",
    "node", "edge", "search", "score", "rank", "load", "save", "update", "delete", "create", "find",
];

/// Deterministic synthetic text of `words` words, varied by `seed`.
fn synthetic(seed: usize, words: usize) -> String {
    let mut x = (seed as u64).wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    let mut out = String::with_capacity(words * 7);
    for i in 0..words {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        if i > 0 {
            out.push(' ');
        }
        out.push_str(WORDS[(x >> 33) as usize % WORDS.len()]);
    }
    out
}

fn bench(embedder: &dyn Embedder, n: usize, words: usize, runs: usize, load_ms: f64) -> anyhow::Result<Value> {
    let inputs: Vec<EmbedInput> = (0..n).map(|i| EmbedInput::document(synthetic(i, words))).collect();
    let total_tokens: usize = inputs.iter().map(|i| embedder.token_count(i)).sum();
    // Warm-up: kernels compile and buffers allocate on the first call.
    embedder.embed(&inputs[..inputs.len().min(32)])?;
    let mut secs = Vec::with_capacity(runs.max(1));
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        embedder.embed(&inputs)?;
        secs.push(t.elapsed().as_secs_f64());
    }
    let median = median(&mut secs.clone());
    Ok(json!({
        "meta": embedder.meta(),
        "load_ms": round2(load_ms),
        "inputs": n,
        "tokens_total": total_tokens,
        "tokens_mean": round2(total_tokens as f64 / n.max(1) as f64),
        "runs_secs": secs.iter().map(|s| round2(*s)).collect::<Vec<_>>(),
        "median_secs": round2(median),
        "tokens_per_sec": round2(total_tokens as f64 / median),
        "texts_per_sec": round2(n as f64 / median),
    }))
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => f64::midpoint(v[n / 2 - 1], v[n / 2]),
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_round_trip() {
        let line = r#"{"id":"a/b.rs#f","text":"fn f() {}","query":true}"#;
        let rec: Record = serde_json::from_str(line).unwrap();
        assert_eq!(rec.id, json!("a/b.rs#f"));
        assert!(rec.query);
        let doc: Record = serde_json::from_str(r#"{"id":7,"text":"x"}"#).unwrap();
        assert!(!doc.query);
        assert_eq!(id_line(&doc.id), "7");

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("v.jsonl");
        let mut sink = Sink::open(out.to_str().unwrap(), Format::Jsonl).unwrap();
        sink.write(&rec.id, &[0.5, -0.25]).unwrap();
        sink.finish().unwrap();
        let back: Value = serde_json::from_str(std::fs::read_to_string(&out).unwrap().trim()).unwrap();
        assert_eq!(back, json!({"id": "a/b.rs#f", "vector": [0.5, -0.25]}));
    }

    #[test]
    fn f32le_writes_rows_and_ids() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("v.f32");
        let mut sink = Sink::open(out.to_str().unwrap(), Format::F32le).unwrap();
        sink.write(&json!("x"), &[1.0, 2.0]).unwrap();
        sink.write(&json!(3), &[3.0, 4.0]).unwrap();
        sink.finish().unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let floats: Vec<f32> = bytes.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(floats, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(std::fs::read_to_string(dir.path().join("v.f32.ids")).unwrap(), "x\n3\n");
        assert!(Sink::open("-", Format::F32le).is_err());
    }

    #[test]
    fn synthetic_text_is_deterministic() {
        assert_eq!(synthetic(3, 10), synthetic(3, 10));
        assert_ne!(synthetic(3, 10), synthetic(4, 10));
        assert_eq!(synthetic(1, 5).split(' ').count(), 5);
        assert!((median(&mut [3.0, 1.0, 2.0]) - 2.0).abs() < f64::EPSILON);
    }
}
