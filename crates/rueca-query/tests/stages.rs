//! The second half of `search_symbols` (RFC 0007 §7.3–§7.5) on the demo
//! snapshot: graph expansion, the centrality prior and the reranker stage
//! with a scripted reranker, in lexical mode (the stages run after any
//! first stage).

mod common;

use std::sync::{Arc, Mutex};

use rueca_embed::Reranker;
use rueca_query::{Engine, ExpandEdges, GraphParams, RerankParams, RerankerLoader};
use serde_json::{Value, json};

use common::*;

/// (qualified name, score) in rank order.
fn ranked(v: &Value) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["symbols"].as_array().unwrap().iter())
        .map(|s| (s["qn"].as_str().unwrap().to_string(), s["score"].as_f64().unwrap()))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out
}

fn names(v: &Value) -> Vec<String> {
    ranked(v).into_iter().map(|(n, _)| n).collect()
}

fn score(v: &Value, qn: &str) -> Option<f64> {
    ranked(v).into_iter().find(|(n, _)| n == qn).map(|(_, s)| s)
}

fn search(e: &Engine, mut args: Value) -> Value {
    args["scores"] = json!(true);
    args["mode"] = json!("lexical");
    e.call("search_symbols", args.clone()).unwrap_or_else(|err| panic!("{args}: {err:#}"))
}

fn engine(f: &Fixture, graph: GraphParams) -> Engine {
    Engine::new(f.store.clone(), Some(f.root.clone())).unwrap().with_graph(graph)
}

fn expand(edges: ExpandEdges) -> GraphParams {
    GraphParams { expand: edges, ..GraphParams::default() }
}

#[test]
fn expansion_adds_one_hop_neighbours_with_a_decayed_score() {
    let f = fixture();
    // BM25 finds the caller and its class through the body text, far below.
    let off = search(&engine(&f, GraphParams::default()), json!({"query": "formatAmount"}));
    let request = "src/util/index:Session.request";
    assert_eq!(names(&off)[0], "src/util/index:formatAmount", "{off}");
    assert!(off["retrieval"].get("expansion").is_none(), "{off}");
    let seed = score(&off, "src/util/index:formatAmount").unwrap();
    assert!(score(&off, request).unwrap() < 0.1 * seed, "{off}");

    // A caller already in the pool keeps the higher of its score and half
    // its seed's (Session.request calls formatAmount); nothing is added.
    let calls = search(&engine(&f, expand(ExpandEdges::Calls)), json!({"query": "formatAmount"}));
    assert_eq!(names(&calls)[..2], ["src/util/index:formatAmount", request], "{calls}");
    assert_eq!(calls["retrieval"]["expansion"], json!({"edges": "calls", "added": 0}));
    let nb = score(&calls, request).unwrap();
    assert!((nb / seed - 0.5).abs() < 0.01, "decay 0.5: {seed} {nb}");

    // A neighbour outside the pool is added: only helper's body says
    // getcwd; InvoiceService.issue calls it.
    let issue = "pkg.service.InvoiceService.issue";
    let alone = search(&engine(&f, GraphParams::default()), json!({"query": "getcwd"}));
    assert_eq!(names(&alone), ["pkg.service.helper"], "{alone}");
    let v = search(&engine(&f, expand(ExpandEdges::Calls)), json!({"query": "getcwd"}));
    assert_eq!(names(&v), ["pkg.service.helper", issue], "{v}");
    assert_eq!(v["retrieval"]["expansion"]["added"], 1, "{v}");
    let (s, n) = (score(&v, "pkg.service.helper").unwrap(), score(&v, issue).unwrap());
    assert!((n / s - 0.5).abs() < 0.01, "{v}");

    // One hop only: Session (request's container) is two away, its score
    // is its own.
    let contains = search(&engine(&f, expand(ExpandEdges::CallsContains)), json!({"query": "formatAmount"}));
    let session = "src/util/index:Session";
    assert_eq!(score(&contains, session), score(&off, session), "{contains}");

    // Decay 0.25.
    let g = GraphParams { decay: 0.25, ..expand(ExpandEdges::Calls) };
    let v = search(&engine(&f, g), json!({"query": "formatAmount"}));
    let (s, n) =
        (score(&v, "src/util/index:formatAmount").unwrap(), score(&v, "src/util/index:Session.request").unwrap());
    assert!((n / s - 0.25).abs() < 0.01, "{s} {n}");
}

#[test]
fn expansion_follows_containment_and_respects_filters_and_caps() {
    let f = fixture();
    let q = json!({"query": "issue invoice", "limit": 20});
    let calls = names(&search(&engine(&f, expand(ExpandEdges::Calls)), q.clone()));
    // InvoiceService.issue's callees (helper, Base.run) join.
    for qn in ["pkg.service.helper", "pkg.service.Base.run"] {
        assert!(calls.contains(&qn.to_string()), "{qn}: {calls:?}");
    }
    let contains = names(&search(&engine(&f, expand(ExpandEdges::CallsContains)), q.clone()));
    // test_issue (a seed) has no members; Base.run's container is a 2nd hop
    // unless Base.run is a seed, so check the method's container instead:
    // InvoiceService defines issue.
    assert!(contains.contains(&"pkg.service.InvoiceService".to_string()), "{contains:?}");

    // Filters apply to neighbours: methods only.
    let methods = search(
        &engine(&f, expand(ExpandEdges::Calls)),
        json!({"query": "issue invoice", "labels": ["Method"], "limit": 20}),
    );
    assert!(!names(&methods).contains(&"pkg.service.helper".to_string()), "{methods}");
    assert!(names(&methods).contains(&"pkg.service.Base.run".to_string()), "{methods}");

    // At most one neighbour per seed, and none at all.
    let one = GraphParams { per_seed: 1, ..expand(ExpandEdges::Calls) };
    let v = search(&engine(&f, one), q.clone());
    let added = v["retrieval"]["expansion"]["added"].as_u64().unwrap();
    let all = search(&engine(&f, expand(ExpandEdges::Calls)), q.clone());
    assert!(added < all["retrieval"]["expansion"]["added"].as_u64().unwrap(), "{v}\n{all}");
    let none = GraphParams { total: 0, ..expand(ExpandEdges::Calls) };
    let v = search(&engine(&f, none), q);
    assert!(v["retrieval"].get("expansion").is_none() || v["retrieval"]["expansion"]["added"] == 0, "{v}");
}

#[test]
fn centrality_prior_weighs_referenced_symbols() {
    let f = fixture();
    let q = json!({"query": "helper", "limit": 5});
    let plain = search(&engine(&f, GraphParams::default()), q.clone());
    let g = GraphParams { centrality_beta: 1.0, ..GraphParams::default() };
    let central = search(&engine(&f, g), q);
    assert_eq!(central["retrieval"]["centrality"], 1.0);
    // pkg.service.helper is called (5 of 10 symbols have a referrer: p =
    // 5/9); src/util/index:helper is not (p = 0).
    let ratio = |qn: &str| score(&central, qn).unwrap() / score(&plain, qn).unwrap();
    assert!((ratio("pkg.service.helper") - (0.25 + 5.0 / 9.0) / 1.25).abs() < 0.01, "{plain}\n{central}");
    assert!((ratio("src/util/index:helper") - 0.2).abs() < 0.01, "{plain}\n{central}");
    assert_eq!(names(&central)[0], "pkg.service.helper");
}

/// Scores a document 1 when it contains `word`, else below 0.5 in input
/// order; records the texts.
struct WordReranker {
    word: &'static str,
    seen: Mutex<Vec<String>>,
}

impl Reranker for WordReranker {
    fn rerank(&self, _query: &str, docs: &[&str]) -> rueca_embed::Result<Vec<f32>> {
        self.seen.lock().unwrap().extend(docs.iter().map(|d| d.to_string()));
        Ok(docs
            .iter()
            .enumerate()
            .map(|(i, d)| if d.contains(self.word) { 1.0 } else { 0.5 - 0.01 * i as f32 })
            .collect())
    }
    fn preset(&self) -> &str {
        "word"
    }
    fn model_key(&self) -> &str {
        "0123456789abcdef0123"
    }
}

struct Loader(Option<Arc<WordReranker>>);

impl RerankerLoader for Loader {
    fn load(&self, preset: &str) -> anyhow::Result<Arc<dyn Reranker>> {
        match &self.0 {
            Some(r) => Ok(r.clone()),
            None => {
                anyhow::bail!("weights for preset `{preset}` are missing; run `rueca models import {preset} <file>`")
            }
        }
    }
}

#[test]
fn reranker_reorders_the_top_k_from_canonical_texts() {
    let f = fixture();
    let word = Arc::new(WordReranker { word: "getcwd", seen: Mutex::new(Vec::new()) });
    let e = engine(&f, GraphParams::default()).with_reranker(Arc::new(Loader(Some(word.clone()))));
    let q = json!({"query": "issue invoice helper", "limit": 10});
    let first = names(&search(&e, q.clone()));
    assert_ne!(first[0], "pkg.service.helper", "{first:?}");
    let mut args = q.clone();
    args["rerank"] = json!(true);
    let v = search(&e, args.clone());
    let reranked = names(&v);
    // helper's body calls os.getcwd: the reranker puts it first; the rest
    // keep their first-stage order.
    assert_eq!(reranked[0], "pkg.service.helper", "{v}");
    let rest: Vec<&String> = first.iter().filter(|n| *n != "pkg.service.helper").collect();
    assert_eq!(reranked[1..].iter().collect::<Vec<_>>(), rest);
    let r = &v["retrieval"]["reranker"];
    assert_eq!((r["preset"].as_str(), r["model_key"].as_str()), (Some("word"), Some("01234567")), "{v}");
    assert_eq!(r["k"].as_u64(), Some(first.len() as u64));
    // The reranker saw canonical texts: header, path, signature, body.
    let seen = word.seen.lock().unwrap().clone();
    let helper = seen.iter().find(|t| t.contains("pkg.service.helper")).unwrap();
    assert!(helper.starts_with("python Function pkg.service.helper\npath: pkg/service.py\n"), "{helper}");
    assert!(helper.contains("return os.getcwd()"), "{helper}");

    // K = 1 reranks the first result only.
    args["rerank_k"] = json!(1);
    let v = search(&e, args);
    assert_eq!(names(&v), first, "{v}");
    assert_eq!(v["retrieval"]["reranker"]["k"], 1);
}

#[test]
fn reranker_fallbacks() {
    let f = fixture();
    let q = json!({"query": "issue invoice helper"});
    let missing = || engine(&f, GraphParams::default()).with_reranker(Arc::new(Loader(None)));
    // Explicit: an error naming the import command.
    let mut args = q.clone();
    args["rerank"] = json!(true);
    let err = format!("{:#}", missing().call("search_symbols", args).unwrap_err());
    assert!(err.contains("reranking unavailable") && err.contains("rueca models import"), "{err}");
    // From configuration: a warning, first-stage order.
    let on = RerankParams { default_on: true, ..RerankParams::default() };
    let v = search(&missing().with_rerank(on.clone()), q.clone());
    let w = v["retrieval"]["warnings"][0].as_str().unwrap();
    assert!(w.starts_with("reranking unavailable, first-stage order kept"), "{v}");
    assert!(v["retrieval"].get("reranker").is_none());
    // `rerank: false` turns a configured reranker off.
    let mut args = q;
    args["rerank"] = json!(false);
    let v = search(&missing().with_rerank(on), args);
    assert!(v["retrieval"].get("warnings").is_none(), "{v}");
}
