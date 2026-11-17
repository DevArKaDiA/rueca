//! The reranker against real weights (RFC 0007 §7.5). Runs only with
//! `RUECA_EMBED_TESTS=1` and the `qwen3-reranker-0.6b-q8` file in the cache
//! (a local conversion registered with `rueca models import`; the cache is
//! `$RUECA_MODEL_DIR`, else `$RUECA_HOME/models`, else
//! `~/.cache/rueca/models`); otherwise the tests pass without running.

#![cfg(feature = "embeddings")]

use std::path::PathBuf;
use std::time::Instant;

use rueca_embed::{DEFAULT_RERANK_PRESET, ModelCache, RerankOptions, Reranker};

/// (query, documents, reference p(yes)): Qwen3-Reranker-0.6B in float32 with
/// `transformers` (the model card's recipe), revision e61197ed, instruction
/// [`rueca_embed::presets::RERANK_INSTRUCTION`].
const CASES: &[(&str, [&str; 5], [f32; 5])] = &[
    (
        "where is the user password hashed before it is stored",
        [
            "fn hash_password(plain: &str, salt: &[u8]) -> String\nHashes a password with argon2 before it is written to the users table.",
            "fn render_login_page(ctx: &Context) -> Html\nRenders the login form.",
            "fn store_user(db: &Db, user: &User) -> Result<()>\nInserts a user row; the password field must already be hashed.",
            "struct Config { port: u16, host: String }\nServer configuration.",
            "fn parse_csv(line: &str) -> Vec<String>\nSplits a CSV line into fields.",
        ],
        [9.973426e-01, 2.804096e-03, 9.961725e-01, 2.100421e-04, 1.032537e-03],
    ),
    (
        "retry an HTTP request with exponential backoff",
        [
            "def fetch_with_retry(url, attempts=5):\n    \"\"\"GET url, retrying with exponential backoff on 5xx and timeouts.\"\"\"",
            "def backoff_delay(attempt, base=0.5):\n    \"\"\"Delay before the next attempt: base * 2**attempt with jitter.\"\"\"",
            "class HttpClient:\n    \"\"\"Thin wrapper around urllib3 with a connection pool.\"\"\"",
            "def sort_by_date(rows):\n    \"\"\"Sorts rows by their created_at column.\"\"\"",
            "README: Installation\nRun pip install mypkg to install.",
        ],
        [9.984947e-01, 7.481894e-01, 1.118167e-02, 5.369542e-05, 2.413061e-05],
    ),
    (
        "compute the BM25 score of a term in a document",
        [
            "fn bm25(tf: f32, df: u32, n_docs: u32, dl: f32, avgdl: f32, k1: f32, b: f32) -> f32\nOkapi BM25 term weight.",
            "fn idf(df: u32, n_docs: u32) -> f32\nInverse document frequency used by the BM25 scorer.",
            "fn tokenize(text: &str) -> Vec<Token>\nSplits text into lowercase tokens.",
            "fn open_segment(path: &Path) -> Result<Segment>\nMaps a segment file into memory.",
            "fn draw_chart(data: &[f64])\nDraws a bar chart in the terminal.",
        ],
        [9.987937e-01, 9.378182e-01, 4.308044e-03, 1.384202e-05, 1.647323e-05],
    ),
    (
        "graceful shutdown when SIGTERM is received",
        [
            "func handleSignals(ctx context.Context, srv *http.Server)\n// Waits for SIGINT or SIGTERM and shuts the server down gracefully.",
            "func (s *Server) Shutdown(ctx context.Context) error\n// Stops accepting connections and waits for in-flight requests.",
            "func main()\n// Parses flags, starts the HTTP server.",
            "func ParseDuration(s string) (time.Duration, error)\n// Parses a duration like 5s.",
            "type Logger interface { Info(msg string) }\n// Logging interface.",
        ],
        [9.953371e-01, 5.876538e-01, 1.730352e-03, 1.597154e-04, 5.480953e-03],
    ),
    (
        "which function deduplicates rows by primary key keeping the latest version",
        [
            "def dedup_latest(df, key, order_col):\n    \"\"\"Keeps the latest row per key, ordered by order_col descending.\"\"\"",
            "def drop_deleted(df):\n    \"\"\"Removes rows whose op column is D.\"\"\"",
            "def read_parquet(path):\n    \"\"\"Reads a parquet file into a DataFrame.\"\"\"",
            "def plot_histogram(values):\n    \"\"\"Plots a histogram of values.\"\"\"",
            "def send_email(to, subject, body):\n    \"\"\"Sends an email through SMTP.\"\"\"",
        ],
        [9.998400e-01, 2.472939e-02, 4.645336e-03, 2.071597e-04, 2.764612e-05],
    ),
    (
        "React component that shows a modal confirmation dialog",
        [
            "export function ConfirmDialog({ open, onConfirm, onCancel, message }: Props)\n// Modal dialog asking the user to confirm an action.",
            "export function Modal({ children, open }: ModalProps)\n// Generic modal container with a backdrop.",
            "export function useFetch<T>(url: string): T | undefined\n// Hook that fetches JSON.",
            "export const theme = { primary: '#6b21a8' }\n// Colour theme.",
            "export function formatCurrency(n: number): string\n// Formats a number as MXN.",
        ],
        [9.971598e-01, 3.315573e-01, 3.762961e-05, 1.919019e-03, 3.078309e-05],
    ),
];

fn store() -> PathBuf {
    match std::env::var_os("RUECA_HOME") {
        Some(home) => PathBuf::from(home),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/rueca"),
    }
}

fn reranker(opts: &RerankOptions) -> Option<Box<dyn Reranker>> {
    if std::env::var("RUECA_EMBED_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipped: set RUECA_EMBED_TESTS=1 with the qwen3-reranker-0.6b-q8 weights imported");
        return None;
    }
    let cache = ModelCache::for_store(&store());
    let preset = rueca_embed::preset(DEFAULT_RERANK_PRESET).expect("preset");
    Some(rueca_embed::backend::load_reranker(&cache, preset, opts).expect("weights imported and loadable"))
}

/// Indices by score, highest first.
fn order(scores: &[f32]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    idx
}

fn logit(p: f32) -> f64 {
    let p = f64::from(p).clamp(1e-9, 1.0 - 1e-9);
    (p / (1.0 - p)).ln()
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let cov: f64 = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f64 = a.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = b.iter().map(|y| (y - mb).powi(2)).sum();
    cov / (va * vb).sqrt()
}

#[test]
fn matches_the_transformers_reference() {
    let Some(r) = reranker(&RerankOptions::default()) else { return };
    let (mut ours, mut refs) = (Vec::new(), Vec::new());
    for (query, docs, reference) in CASES {
        let scores = r.rerank(query, docs).expect("rerank");
        eprintln!("{query}\n  rueca     {scores:?}\n  reference {reference:?}");
        assert_eq!(order(&scores), order(reference), "ranking of `{query}`");
        ours.extend(scores.iter().copied());
        refs.extend(reference.iter().copied());
    }
    let (lo, lr): (Vec<f64>, Vec<f64>) =
        (ours.iter().map(|&p| logit(p)).collect(), refs.iter().map(|&p| logit(p)).collect());
    let p = |v: &[f32]| v.iter().map(|&x| f64::from(x)).collect::<Vec<_>>();
    let (r_logit, r_prob) = (pearson(&lo, &lr), pearson(&p(&ours), &p(&refs)));
    let max_diff = ours.iter().zip(&refs).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
    eprintln!("Pearson r: log-odds {r_logit:.4}, probability {r_prob:.4}; max |Δp| {max_diff:.4}");
    assert!(r_logit > 0.98 && r_prob > 0.98, "log-odds {r_logit}, probability {r_prob}");
}

#[test]
fn scores_do_not_depend_on_order_or_chunking() {
    let Some(r) = reranker(&RerankOptions::default()) else { return };
    let Some(one) = reranker(&RerankOptions { batch_tokens: 1, ..RerankOptions::default() }) else { return };
    let (query, docs, _) = CASES[0];
    let forward = r.rerank(query, &docs).expect("rerank");
    let mut reversed_docs = docs;
    reversed_docs.reverse();
    let mut reversed = r.rerank(query, &reversed_docs).expect("rerank");
    reversed.reverse();
    let alone = one.rerank(query, &docs).expect("rerank");
    for ((a, b), c) in forward.iter().zip(&reversed).zip(&alone) {
        assert!((a - b).abs() < 2e-3 && (a - c).abs() < 2e-3, "{a} {b} {c}");
    }
    assert!(r.rerank(query, &[]).expect("rerank").is_empty());
    assert_eq!(r.preset(), DEFAULT_RERANK_PRESET);
    assert_eq!(r.model_key().len(), 64);
}

/// A canonical-text-like document of about 256 tokens.
fn synthetic_doc(i: usize) -> String {
    let mut s = format!(
        "rust Function crate::module{i}::handle_request_{i}\npath: src/module{i}/handler.rs\n\
         pub fn handle_request_{i}(req: &Request, store: &Store) -> Result<Response>\n\
         /// Handles request {i}: validates the session, loads the account and renders the page.\n"
    );
    while s.len() < 1000 {
        s.push_str(&format!("let account_{i} = store.load_account(req.user_id)?; validate(&account_{i})?;\n"));
    }
    s
}

/// Reranker latency for K = 20 and 50 documents of about 256 tokens, on
/// the GPU (Metal) and with `cpu`, median of three warm runs.
/// `RUECA_EMBED_TESTS=1 cargo test -p rueca-embed --test rerank latency -- --ignored --nocapture`
#[test]
#[ignore = "latency measurement"]
fn latency_k20_k50() {
    let query = "where is the request handler that validates the session and loads the account";
    // `RUECA_RERANK_BATCH` overrides the tokens per decode call.
    let batch_tokens = std::env::var("RUECA_RERANK_BATCH").ok().and_then(|v| v.parse().ok());
    let cpus: &[bool] = if std::env::var_os("RUECA_RERANK_GPU_ONLY").is_some() { &[false] } else { &[false, true] };
    for &cpu in cpus {
        let mut opts = RerankOptions { cpu, ..RerankOptions::default() };
        opts.batch_tokens = batch_tokens.unwrap_or(opts.batch_tokens);
        let Some(r) = reranker(&opts) else { return };
        for k in [20, 50] {
            let docs: Vec<String> = (0..k).map(synthetic_doc).collect();
            let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
            r.rerank(query, &refs).expect("warm-up");
            let mut ms: Vec<f64> = (0..3)
                .map(|_| {
                    let t = Instant::now();
                    r.rerank(query, &refs).expect("rerank");
                    t.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            ms.sort_by(f64::total_cmp);
            let device = if cpu { "cpu" } else { "gpu" };
            println!("reranker {device} K={k} batch {}: {:.0} ms", opts.batch_tokens, ms[1]);
        }
    }
}
