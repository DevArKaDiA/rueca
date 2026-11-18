//! Remote providers against a local mock server (no real API is called).
#![cfg(feature = "remote")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rueca_embed::remote::{Backoff, Endpoint, RemoteEmbedder, RemoteReranker, RemoteSpec};
use rueca_embed::{EmbedInput, EmbedOptions, Embedder, Reranker};
use serde_json::{Value, json};

/// One request as the mock saw it.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}

/// What the mock answers: status, extra headers, body.
type Reply = (u16, Vec<(&'static str, String)>, String);

/// A one-thread HTTP/1.1 server answering each request with `handler`.
fn serve(handler: impl Fn(usize, &Seen) -> Reply + Send + 'static) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { continue };
            let req = read_request(&mut stream);
            log.lock().unwrap().push(req.clone());
            let (status, headers, body) = handler(n, &req);
            let mut out = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(&body);
            let _ = stream.write_all(out.as_bytes());
        }
    });
    (url, seen)
}

fn read_request(stream: &mut std::net::TcpStream) -> Seen {
    let mut r = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let path = line.split_whitespace().nth(1).unwrap_or_default().to_string();
    let mut headers = Vec::new();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap();
        let (k, v) = (k.trim().to_lowercase(), v.trim().to_string());
        if k == "content-length" {
            len = v.parse().unwrap();
        }
        headers.push((k, v));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();
    Seen { path, headers, body: serde_json::from_slice(&body).unwrap_or(Value::Null) }
}

fn endpoint(url: &str) -> Endpoint {
    Endpoint {
        base_url: url.to_string(),
        api_key: "test-key".into(),
        backoff: Backoff { attempts: 4, base: Duration::from_millis(5), max: Duration::from_millis(20) },
    }
}

fn header<'a>(s: &'a Seen, k: &str) -> Option<&'a str> {
    s.headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.as_str())
}

/// `n` vectors of width `dims`: vector i is 1 at i % dims (scaled by 2 so
/// normalization shows).
fn vectors(n: usize, dims: usize) -> Vec<Vec<f32>> {
    (0..n)
        .map(|i| {
            let mut v = vec![0.0; dims];
            v[i % dims] = 2.0;
            v
        })
        .collect()
}

#[test]
fn voyage_embeds_with_input_type_dimension_and_retries() {
    let (url, seen) = serve(|n, req| {
        if n == 0 {
            return (429, vec![("retry-after", "0".into())], "{\"detail\":\"slow down\"}".into());
        }
        if n == 1 {
            return (503, vec![], "{}".into());
        }
        let k = req.body["input"].as_array().unwrap().len();
        let data: Vec<Value> =
            vectors(k, 512).into_iter().enumerate().rev().map(|(i, e)| json!({"index": i, "embedding": e})).collect();
        (200, vec![], json!({"data": data, "usage": {"total_tokens": 10}}).to_string())
    });
    let spec = RemoteSpec::parse("remote:voyage/voyage-code-3").unwrap().unwrap();
    let opts = EmbedOptions { dims: Some(512), ..EmbedOptions::default() };
    let e = RemoteEmbedder::new(spec, endpoint(&url), &opts).unwrap();
    let out =
        e.embed(&[EmbedInput::document("fn a()"), EmbedInput::query("where is a"), EmbedInput::document("b")]).unwrap();
    assert_eq!(out.len(), 3);
    // Documents went in one request (indices 0, 1), the query in another.
    assert_eq!(out[0][0], 1.0);
    assert_eq!(out[2][1], 1.0);
    assert_eq!(out[1][0], 1.0);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4, "two retries, then two requests");
    let docs = &seen[2];
    assert_eq!(docs.path, "/v1/embeddings");
    assert_eq!(header(docs, "authorization"), Some("Bearer test-key"));
    assert_eq!(docs.body["input_type"], "document");
    assert_eq!(docs.body["output_dimension"], 512);
    assert_eq!(docs.body["model"], "voyage-code-3");
    assert_eq!(seen[3].body["input_type"], "query");
}

#[test]
fn openai_sends_dimensions_and_rejects_a_wrong_width() {
    let (url, seen) = serve(|_, req| {
        let k = req.body["input"].as_array().unwrap().len();
        let data: Vec<Value> =
            vectors(k, 256).into_iter().enumerate().map(|(i, e)| json!({"index": i, "embedding": e})).collect();
        (200, vec![], json!({"data": data}).to_string())
    });
    let spec = RemoteSpec::parse("remote:openai/text-embedding-3-large").unwrap().unwrap();
    let ok =
        RemoteEmbedder::new(spec.clone(), endpoint(&url), &EmbedOptions { dims: Some(256), ..EmbedOptions::default() })
            .unwrap();
    assert_eq!(ok.embed_query("q").unwrap().len(), 256);
    assert_eq!(seen.lock().unwrap()[0].body["dimensions"], 256);
    assert!(seen.lock().unwrap()[0].body.get("input_type").is_none());
    // Full width asked, 256 returned: an error naming `dims`.
    let full = RemoteEmbedder::new(spec, endpoint(&url), &EmbedOptions::default()).unwrap();
    let err = full.embed_query("q").unwrap_err().to_string();
    assert!(err.contains("expected 3072"), "{err}");
}

#[test]
fn gemini_batches_with_task_types_and_key_header() {
    let (url, seen) = serve(|_, req| {
        let k = req.body["requests"].as_array().unwrap().len();
        let embeddings: Vec<Value> = vectors(k, 768).into_iter().map(|v| json!({"values": v})).collect();
        (200, vec![], json!({"embeddings": embeddings}).to_string())
    });
    let spec = RemoteSpec::parse("remote:gemini/gemini-embedding-001").unwrap().unwrap();
    let e = RemoteEmbedder::new(spec, endpoint(&url), &EmbedOptions { dims: Some(768), ..EmbedOptions::default() })
        .unwrap();
    let docs: Vec<String> = (0..150).map(|i| format!("doc {i}")).collect();
    let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let out = e.embed_documents(&refs).unwrap();
    assert_eq!(out.len(), 150);
    assert_eq!(out[149][49], 1.0, "the second batch holds documents 100..150");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "100 per batch");
    assert_eq!(seen[0].path, "/v1beta/models/gemini-embedding-001:batchEmbedContents");
    assert_eq!(header(&seen[0], "x-goog-api-key"), Some("test-key"));
    let r0 = &seen[0].body["requests"][0];
    assert_eq!(r0["taskType"], "RETRIEVAL_DOCUMENT");
    assert_eq!(r0["outputDimensionality"], 768);
    assert_eq!(r0["model"], "models/gemini-embedding-001");
}

#[test]
fn rerankers_return_scores_in_document_order() {
    let (url, seen) = serve(|_, req| {
        let n = req.body["documents"].as_array().unwrap().len();
        let rows: Vec<Value> = (0..n).rev().map(|i| json!({"index": i, "relevance_score": 0.1 * i as f64})).collect();
        let list = if req.path == "/v2/rerank" { "results" } else { "data" };
        (200, vec![], json!({ list: rows }).to_string())
    });
    for name in ["remote:voyage/rerank-2.5", "remote:cohere/rerank-v3.5"] {
        let r = RemoteReranker::new(RemoteSpec::parse(name).unwrap().unwrap(), endpoint(&url)).unwrap();
        let s = r.rerank("query", &["a", "b", "c"]).unwrap();
        assert_eq!(s.len(), 3);
        assert!(s[2] > s[1] && s[1] > s[0], "{name}: {s:?}");
        assert_eq!(r.preset(), name);
        assert_eq!(r.model_key().len(), 64);
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].path, "/v1/rerank");
    assert_eq!(seen[1].path, "/v2/rerank");
    assert_eq!(seen[1].body["model"], "rerank-v3.5");
}

#[test]
fn client_errors_fail_without_retrying() {
    let (url, seen) = serve(|_, _| (401, vec![], "{\"detail\":\"invalid key\"}".into()));
    let spec = RemoteSpec::parse("remote:voyage/voyage-code-3").unwrap().unwrap();
    let e = RemoteEmbedder::new(spec, endpoint(&url), &EmbedOptions::default()).unwrap();
    let err = e.embed_query("q").unwrap_err().to_string();
    assert!(err.contains("401") && err.contains("invalid key"), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}
