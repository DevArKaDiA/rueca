//! `rueca rerank`: scores documents against a query with a reranker preset
//! (RFC 0007 §7.5), for the bench and for debugging.
//!
//! Input is JSON lines `{"id": ..., "text": "..."}`; output is JSON lines
//! `{"id": ..., "score": p}` in input order, `p` the reranker's relevance
//! in `[0, 1]`. All documents of one input are scored in one call, so the
//! shared prompt prefix is computed once.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use clap::Args;
use rueca_embed::{ModelCache, RerankOptions};
use serde::Deserialize;
use serde_json::{Value, json};

/// Arguments of `rueca rerank`.
#[derive(Args, Debug)]
pub struct RerankArgs {
    /// Reranker preset (`rueca models list`).
    #[arg(long, env = "RUECA_RERANK_PRESET", default_value = rueca_embed::DEFAULT_RERANK_PRESET)]
    preset: String,
    /// The query.
    #[arg(long, value_name = "TEXT")]
    query: String,
    /// JSON lines input: `{"id": ..., "text": "..."}`.
    #[arg(long, value_name = "FILE|-", default_value = "-")]
    input: String,
    /// Output (JSON lines `{"id": ..., "score": ...}`).
    #[arg(long, value_name = "FILE|-", default_value = "-")]
    output: String,
    /// Tokens of each document, at most.
    #[arg(long, value_name = "N", default_value_t = rueca_embed::DEFAULT_RERANK_DOC_TOKENS)]
    max_tokens: usize,
    /// Document tokens per decode call.
    #[arg(long, value_name = "N", default_value_t = rueca_embed::DEFAULT_RERANK_BATCH_TOKENS)]
    batch: usize,
    /// Run on the CPU even when a GPU backend is available.
    #[arg(long)]
    cpu: bool,
    /// Worker threads (default: physical cores).
    #[arg(long, value_name = "N")]
    threads: Option<usize>,
    /// The prompt's instruction (default: the preset's).
    #[arg(long, value_name = "TEXT")]
    instruction: Option<String>,
    /// Also print `{"load_ms", "rerank_ms", "docs", "preset", "model_key"}` to stderr.
    #[arg(long)]
    timing: bool,
}

#[derive(Debug, Deserialize)]
struct Record {
    id: Value,
    text: String,
}

/// Runs `rueca rerank`.
pub fn run(store: &Path, args: &RerankArgs) -> anyhow::Result<()> {
    let records = read(&args.input)?;
    let preset = rueca_embed::preset(&args.preset)?;
    let opts = RerankOptions {
        max_doc_tokens: args.max_tokens,
        batch_tokens: args.batch,
        threads: args.threads,
        cpu: args.cpu,
        instruction: args.instruction.clone(),
    };
    let started = Instant::now();
    let reranker = rueca_embed::backend::load_reranker(&ModelCache::for_store(store), preset, &opts)?;
    let load_ms = started.elapsed().as_secs_f64() * 1e3;
    let docs: Vec<&str> = records.iter().map(|r| r.text.as_str()).collect();
    let started = Instant::now();
    let scores = reranker.rerank(&args.query, &docs)?;
    let rerank_ms = started.elapsed().as_secs_f64() * 1e3;
    let mut out: Box<dyn Write> = match args.output.as_str() {
        "-" => Box::new(BufWriter::new(std::io::stdout().lock())),
        path => {
            Box::new(BufWriter::new(File::create(path).map_err(|e| anyhow::anyhow!("cannot create `{path}`: {e}"))?))
        }
    };
    for (r, s) in records.iter().zip(&scores) {
        writeln!(out, "{}", json!({ "id": r.id, "score": s }))?;
    }
    out.flush()?;
    if args.timing {
        let t = json!({
            "preset": reranker.preset(),
            "model_key": reranker.model_key(),
            "docs": docs.len(),
            "load_ms": (load_ms * 100.0).round() / 100.0,
            "rerank_ms": (rerank_ms * 100.0).round() / 100.0,
        });
        eprintln!("{t}");
    }
    Ok(())
}

fn read(input: &str) -> anyhow::Result<Vec<Record>> {
    let reader: Box<dyn BufRead> = match input {
        "-" => Box::new(BufReader::new(std::io::stdin().lock())),
        path => Box::new(BufReader::new(File::open(path).map_err(|e| anyhow::anyhow!("cannot open `{path}`: {e}"))?)),
    };
    let mut out = Vec::new();
    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line).map_err(|e| anyhow::anyhow!("input line {}: not a record: {e}", n + 1))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_jsonl_records() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.jsonl");
        std::fs::write(&p, "{\"id\":1,\"text\":\"fn a()\"}\n\n{\"id\":\"b\",\"text\":\"x\"}\n").unwrap();
        let r = read(p.to_str().unwrap()).unwrap();
        assert_eq!((r.len(), r[1].id.clone(), r[0].text.as_str()), (2, json!("b"), "fn a()"));
        std::fs::write(&p, "{\"id\":1}\n").unwrap();
        assert!(read(p.to_str().unwrap()).unwrap_err().to_string().contains("line 1"));
    }
}
