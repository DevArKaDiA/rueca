//! Remote presets (RFC 0007 §3.5): embedders and rerankers behind a
//! provider's HTTP API, named `remote:<provider>/<model>`.
//!
//! | Provider | Embeddings | Rerank | API key |
//! |---|---|---|---|
//! | `voyage` | `POST /v1/embeddings` (`input_type`, `output_dimension`) | `POST /v1/rerank` | `VOYAGE_API_KEY` |
//! | `openai` | `POST /v1/embeddings` (`dimensions`) | — | `OPENAI_API_KEY` |
//! | `gemini` | `POST /v1beta/models/<model>:batchEmbedContents` (`taskType`, `outputDimensionality`) | — | `GEMINI_API_KEY` |
//! | `cohere` | — | `POST /v2/rerank` | `COHERE_API_KEY` |
//! | `bedrock` | later: needs AWS SigV4 and credential resolution | — | — |
//!
//! Any model of a provider is accepted (`remote:voyage/voyage-3.5`); the
//! manifest ([`crate::PRESETS`]) lists the ones the RFC compared. A base URL
//! override per provider (`RUECA_VOYAGE_BASE_URL`, `RUECA_OPENAI_BASE_URL`,
//! `RUECA_GEMINI_BASE_URL`, `RUECA_COHERE_BASE_URL`) serves proxies and
//! compatible servers.
//!
//! - **Keys.** The embed key of a remote preset hashes provider, model, API
//!   version, output width, text builder version and token budget
//!   ([`crate::EmbedKey::remote`]); providers publish no weights version, so
//!   a provider that changes weights behind a name is not detected.
//! - **Batching** per provider: at most [`Provider::batch_limits`] inputs
//!   and estimated tokens (4 characters per token) per request.
//! - **Retries** on 429, 5xx and transport errors, with exponential backoff
//!   ([`Backoff`]) or the answer's `Retry-After` seconds.
//! - Every vector is L2-normalized on arrival (Gemini's are not below its
//!   full width).
//!
//! The privacy gate (`[retrieval] allow_remote` of `rueca-org.toml`) is
//! the indexer's and the query engine's: this module only talks HTTP.

use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::key::{Backend, EmbedKey, ModelKey, ModelMeta, VectorEncoding};
use crate::presets::{Pooling, Role};
use crate::vector::{l2_normalize, pack_batches};
use crate::{EmbedInput, EmbedOptions, Embedder, InputKind, Reranker};

/// The prefix of every remote preset name.
pub const REMOTE_PREFIX: &str = "remote:";

/// Response bodies are read up to this many bytes.
const MAX_BODY_BYTES: u64 = 256 * 1024 * 1024;

pub use crate::presets::is_remote_preset as is_remote;

/// An embedding or rerank API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Voyage AI.
    Voyage,
    /// OpenAI.
    OpenAi,
    /// Google Gemini API.
    Gemini,
    /// Cohere (rerank).
    Cohere,
    /// Amazon Bedrock (not implemented).
    Bedrock,
}

impl Provider {
    /// The provider of a preset name's `<provider>` part.
    #[must_use]
    pub fn parse(s: &str) -> Option<Provider> {
        Some(match s {
            "voyage" => Provider::Voyage,
            "openai" => Provider::OpenAi,
            "gemini" => Provider::Gemini,
            "cohere" => Provider::Cohere,
            "bedrock" => Provider::Bedrock,
            _ => return None,
        })
    }

    /// The name in preset names.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Voyage => "voyage",
            Provider::OpenAi => "openai",
            Provider::Gemini => "gemini",
            Provider::Cohere => "cohere",
            Provider::Bedrock => "bedrock",
        }
    }

    /// The environment variable holding the API key.
    #[must_use]
    pub fn key_env(self) -> &'static str {
        match self {
            Provider::Voyage => "VOYAGE_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::Gemini => "GEMINI_API_KEY",
            Provider::Cohere => "COHERE_API_KEY",
            Provider::Bedrock => "AWS_PROFILE",
        }
    }

    /// The environment variable overriding the base URL.
    #[must_use]
    pub fn base_url_env(self) -> &'static str {
        match self {
            Provider::Voyage => "RUECA_VOYAGE_BASE_URL",
            Provider::OpenAi => "RUECA_OPENAI_BASE_URL",
            Provider::Gemini => "RUECA_GEMINI_BASE_URL",
            Provider::Cohere => "RUECA_COHERE_BASE_URL",
            Provider::Bedrock => "RUECA_BEDROCK_BASE_URL",
        }
    }

    /// The public endpoint.
    #[must_use]
    pub fn default_base_url(self) -> &'static str {
        match self {
            Provider::Voyage => "https://api.voyageai.com",
            Provider::OpenAi => "https://api.openai.com",
            Provider::Gemini => "https://generativelanguage.googleapis.com",
            Provider::Cohere => "https://api.cohere.com",
            Provider::Bedrock => "https://bedrock-runtime.us-east-1.amazonaws.com",
        }
    }

    /// The API version in the request path, hashed into the embed key.
    #[must_use]
    pub fn api_version(self) -> &'static str {
        match self {
            Provider::Voyage | Provider::OpenAi => "v1",
            Provider::Gemini => "v1beta",
            Provider::Cohere => "v2",
            Provider::Bedrock => "",
        }
    }

    /// Inputs and estimated tokens per embeddings request (under the
    /// documented limits: Voyage 1,000 inputs and 120K tokens for its large
    /// models, OpenAI 2,048 inputs and 300K tokens, Gemini 100 requests per
    /// batch).
    #[must_use]
    pub fn batch_limits(self) -> (usize, usize) {
        match self {
            Provider::Voyage => (256, 100_000),
            Provider::OpenAi => (512, 250_000),
            Provider::Gemini => (100, 200_000),
            Provider::Cohere | Provider::Bedrock => (96, 100_000),
        }
    }
}

/// A parsed `remote:<provider>/<model>` name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    /// The full preset name.
    pub name: String,
    /// The API.
    pub provider: Provider,
    /// The provider's model id.
    pub model: String,
    /// Embedder, or reranker for `rerank*` models.
    pub role: Role,
}

impl RemoteSpec {
    /// Parses a preset name; `Ok(None)` when it is not a `remote:` name.
    ///
    /// # Errors
    ///
    /// An unknown provider or an empty model ([`Error::Invalid`]); Bedrock
    /// and Cohere embeddings, which are not implemented
    /// ([`Error::Unavailable`]).
    pub fn parse(name: &str) -> Result<Option<RemoteSpec>> {
        let Some(rest) = name.strip_prefix(REMOTE_PREFIX) else { return Ok(None) };
        let (p, model) = rest.split_once('/').unwrap_or((rest, ""));
        let provider = Provider::parse(p).ok_or_else(|| {
            Error::Invalid(format!("unknown provider `{p}` in `{name}` (voyage, openai, gemini, cohere)"))
        })?;
        if model.is_empty() {
            return Err(Error::Invalid(format!("`{name}` names no model (`remote:{p}/<model>`)")));
        }
        let role = if model.starts_with("rerank") { Role::Reranker } else { Role::Embedder };
        let later = |reason: &str| Error::Unavailable {
            preset: name.to_string(),
            status: "later".into(),
            reason: reason.to_string(),
        };
        match (provider, role) {
            (Provider::Bedrock, _) => Err(later(
                "Bedrock needs AWS SigV4 signing and credential resolution, not implemented (RFC 0007 §3.5); \
                 use `remote:openai/…` through a compatible gateway",
            )),
            (Provider::Cohere, Role::Embedder) => Err(later("Cohere is implemented for rerank only")),
            (Provider::OpenAi | Provider::Gemini, Role::Reranker) => {
                Err(later("rerank is implemented for `voyage` and `cohere`"))
            }
            _ => Ok(Some(RemoteSpec { name: name.to_string(), provider, model: model.to_string(), role })),
        }
    }

    /// The width the provider returns without an explicit one.
    #[must_use]
    pub fn default_dims(&self) -> usize {
        match self.provider {
            Provider::OpenAi if self.model == "text-embedding-3-large" => 3072,
            Provider::OpenAi => 1536,
            Provider::Gemini if self.model.starts_with("gemini-embedding") => 3072,
            Provider::Gemini => 768,
            _ => 1024,
        }
    }
}

/// Retries of a request on 429, 5xx and transport errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Attempts in all (the first included).
    pub attempts: u32,
    /// Wait before the first retry; doubled after each.
    pub base: Duration,
    /// Longest wait (also caps `Retry-After`).
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { attempts: 5, base: Duration::from_millis(500), max: Duration::from_secs(30) }
    }
}

/// Where and how a provider is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Scheme, host and optional path prefix (no trailing slash needed).
    pub base_url: String,
    /// The API key.
    pub api_key: String,
    /// Retries.
    pub backoff: Backoff,
}

impl Endpoint {
    /// The provider's endpoint: base URL from its override variable or the
    /// public one, key from its key variable.
    ///
    /// # Errors
    ///
    /// The key variable is unset or empty.
    pub fn from_env(provider: Provider) -> Result<Endpoint> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let api_key = var(provider.key_env()).ok_or_else(|| {
            Error::Invalid(format!(
                "remote preset `{}` needs an API key: set {}",
                provider.as_str(),
                provider.key_env()
            ))
        })?;
        let base_url = var(provider.base_url_env()).unwrap_or_else(|| provider.default_base_url().to_string());
        Ok(Endpoint { base_url, api_key, backoff: Backoff::default() })
    }
}

/// The audit record of a remote embedder, without any request: width from
/// `opts.dims`, else the model's default.
///
/// # Errors
///
/// A reranker spec, a zero width or a width outside the manifest's range
/// for a listed model.
pub fn describe(spec: &RemoteSpec, opts: &EmbedOptions) -> Result<ModelMeta> {
    if spec.role != Role::Embedder {
        return Err(Error::Invalid(format!("`{}` is a reranker preset, not an embedder", spec.name)));
    }
    let dims = opts.dims.unwrap_or_else(|| spec.default_dims());
    let range = crate::presets::preset(&spec.name).map_or(1..=usize::MAX, |p| p.min_dims..=p.dims);
    if dims == 0 || !range.contains(&dims) {
        return Err(Error::Invalid(format!("--dims {dims} is outside {range:?} for `{}`", spec.name)));
    }
    let p = spec.provider;
    let embed_key =
        EmbedKey::remote(p.as_str(), &spec.model, p.api_version(), dims, opts.text_builder_version, opts.max_tokens);
    let encoding = VectorEncoding::F32;
    let (_, batch_tokens) = p.batch_limits();
    Ok(ModelMeta {
        preset: spec.name.clone(),
        model: format!("{}/{}", p.as_str(), spec.model),
        model_sha256: String::new(),
        quantization: String::new(),
        dims,
        max_dims: dims,
        pooling: Pooling::Provider,
        query_prefix: String::new(),
        document_prefix: String::new(),
        token_budget: opts.max_tokens,
        text_builder_version: opts.text_builder_version,
        encoding,
        embed_key,
        model_key: ModelKey::new(&embed_key, dims, encoding),
        runtime: "remote".into(),
        runtime_version: format!("{} {}", p.as_str(), p.api_version()),
        backend: Backend::Remote,
        cpu_level: String::new(),
        batch_tokens,
        ubatch_tokens: batch_tokens,
        threads: 1,
    })
}

/// Tokens of a text as estimated without a tokenizer (4 characters each).
fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4) + 1
}

/// One request's outcome.
enum Attempt {
    Done(Value),
    Retry(Option<Duration>, String),
    Fail(String),
}

/// An HTTP client of one provider.
struct Client {
    agent: ureq::Agent,
    provider: Provider,
    endpoint: Endpoint,
}

impl Client {
    fn new(provider: Provider, endpoint: Endpoint) -> Client {
        Client { agent: crate::http::agent(Some(Duration::from_secs(120)), false), provider, endpoint }
    }

    fn error(&self, message: impl Into<String>) -> Error {
        Error::Remote { provider: self.provider.as_str().to_string(), message: message.into() }
    }

    /// POSTs `body` to `path` with retries; the parsed JSON answer.
    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{path}", self.endpoint.base_url.trim_end_matches('/'));
        let payload = body.to_string();
        let b = self.endpoint.backoff;
        let mut wait = b.base;
        let mut last = String::new();
        for attempt in 1..=b.attempts.max(1) {
            match self.send_once(&url, &payload) {
                Attempt::Done(v) => return Ok(v),
                Attempt::Fail(m) => return Err(self.error(m)),
                Attempt::Retry(after, m) => {
                    tracing::debug!(provider = self.provider.as_str(), attempt, "remote: retrying: {m}");
                    last = m;
                    if attempt < b.attempts {
                        std::thread::sleep(after.unwrap_or(wait).min(b.max));
                        wait = (wait * 2).min(b.max);
                    }
                }
            }
        }
        Err(self.error(format!("{last} (after {} attempts)", b.attempts.max(1))))
    }

    fn send_once(&self, url: &str, payload: &str) -> Attempt {
        let key = &self.endpoint.api_key;
        let mut req = self.agent.post(url).header("content-type", "application/json");
        req = match self.provider {
            Provider::Gemini => req.header("x-goog-api-key", key),
            _ => req.header("authorization", &format!("Bearer {key}")),
        };
        let resp = match req.send(payload) {
            Ok(r) => r,
            Err(e) => return Attempt::Retry(None, format!("{url}: {e}")),
        };
        let status = resp.status().as_u16();
        let after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = match resp.into_body().into_with_config().limit(MAX_BODY_BYTES).read_to_string() {
            Ok(t) => t,
            Err(e) => return Attempt::Retry(None, format!("{url}: reading the answer: {e}")),
        };
        classify(status, after, &text, url)
    }
}

/// What a status and body mean: JSON for 2xx, a retry for 429 / 5xx, a
/// failure otherwise (with the provider's message).
fn classify(status: u16, after: Option<Duration>, text: &str, url: &str) -> Attempt {
    let snippet: String = text.chars().take(300).collect();
    match status {
        200..=299 => match serde_json::from_str(text) {
            Ok(v) => Attempt::Done(v),
            Err(e) => Attempt::Fail(format!("{url}: answer is not JSON ({e}): {snippet}")),
        },
        429 | 500..=599 => Attempt::Retry(after, format!("{url}: HTTP {status}: {snippet}")),
        _ => Attempt::Fail(format!("{url}: HTTP {status}: {snippet}")),
    }
}

/// An embedder behind a provider's API.
pub struct RemoteEmbedder {
    spec: RemoteSpec,
    client: Client,
    meta: ModelMeta,
}

impl RemoteEmbedder {
    /// An embedder of `spec` calling `endpoint`, with the width and keys of
    /// [`describe`].
    ///
    /// # Errors
    ///
    /// As [`describe`].
    pub fn new(spec: RemoteSpec, endpoint: Endpoint, opts: &EmbedOptions) -> Result<RemoteEmbedder> {
        let meta = describe(&spec, opts)?;
        let client = Client::new(spec.provider, endpoint);
        Ok(RemoteEmbedder { spec, client, meta })
    }

    /// The request of one batch: path and body.
    fn request(&self, kind: InputKind, texts: &[&str]) -> (String, Value) {
        let dims = self.meta.dims;
        let custom = dims != self.spec.default_dims();
        let model = self.spec.model.as_str();
        match self.spec.provider {
            Provider::Gemini => {
                let task = if kind == InputKind::Query { "CODE_RETRIEVAL_QUERY" } else { "RETRIEVAL_DOCUMENT" };
                let requests: Vec<Value> = texts
                    .iter()
                    .map(|t| {
                        let mut r = json!({"model": format!("models/{model}"), "content": {"parts": [{"text": t}]}, "taskType": task});
                        if custom {
                            r["outputDimensionality"] = json!(dims);
                        }
                        r
                    })
                    .collect();
                (format!("/v1beta/models/{model}:batchEmbedContents"), json!({ "requests": requests }))
            }
            Provider::OpenAi => {
                let mut body = json!({"model": model, "input": texts, "encoding_format": "float"});
                if custom {
                    body["dimensions"] = json!(dims);
                }
                ("/v1/embeddings".into(), body)
            }
            _ => {
                let input_type = if kind == InputKind::Query { "query" } else { "document" };
                let mut body = json!({"model": model, "input": texts, "input_type": input_type, "truncation": true});
                if custom {
                    body["output_dimension"] = json!(dims);
                }
                ("/v1/embeddings".into(), body)
            }
        }
    }

    /// The vectors of an answer, in input order, normalized.
    fn vectors(&self, answer: &Value, n: usize) -> Result<Vec<Vec<f32>>> {
        let rows: Vec<(usize, &Value)> = match self.spec.provider {
            Provider::Gemini => answer["embeddings"]
                .as_array()
                .map(|a| a.iter().enumerate().map(|(i, e)| (i, &e["values"])).collect())
                .unwrap_or_default(),
            _ => answer["data"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .enumerate()
                        .map(|(i, e)| (e["index"].as_u64().map_or(i, |x| x as usize), &e["embedding"]))
                        .collect()
                })
                .unwrap_or_default(),
        };
        let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
        for (i, v) in rows {
            let slot = out.get_mut(i).ok_or_else(|| self.client.error(format!("answer index {i} out of range")))?;
            *slot = Some(self.vector(v)?);
        }
        out.into_iter().map(|v| v.ok_or_else(|| self.client.error(format!("{n} inputs, fewer vectors")))).collect()
    }

    fn vector(&self, v: &Value) -> Result<Vec<f32>> {
        let a = v.as_array().ok_or_else(|| self.client.error("an embedding is not an array"))?;
        #[allow(clippy::cast_possible_truncation)]
        let mut out: Vec<f32> = a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
        if out.len() != self.meta.dims {
            return Err(self.client.error(format!(
                "returned {} dimensions, expected {} (set `dims` to the model's width)",
                out.len(),
                self.meta.dims
            )));
        }
        l2_normalize(&mut out);
        Ok(out)
    }

    /// Embeds texts of one kind, in batches within the provider's limits.
    fn embed_kind(&self, kind: InputKind, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let (items, tokens) = self.spec.provider.batch_limits();
        let lens: Vec<usize> = texts.iter().map(|t| estimate_tokens(t)).collect();
        let mut out = Vec::with_capacity(texts.len());
        for r in pack_batches(&lens, tokens, items) {
            let batch = &texts[r];
            let (path, body) = self.request(kind, batch);
            let answer = self.client.post(&path, &body)?;
            out.extend(self.vectors(&answer, batch.len())?);
        }
        Ok(out)
    }
}

impl Embedder for RemoteEmbedder {
    fn model_key(&self) -> &ModelKey {
        &self.meta.model_key
    }

    fn dims(&self) -> usize {
        self.meta.dims
    }

    fn meta(&self) -> &ModelMeta {
        &self.meta
    }

    fn token_count(&self, input: &EmbedInput) -> usize {
        estimate_tokens(&input.text).min(self.meta.token_budget.max(1))
    }

    fn embed(&self, inputs: &[EmbedInput]) -> Result<Vec<Vec<f32>>> {
        let mut out: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
        for kind in [InputKind::Document, InputKind::Query] {
            let idx: Vec<usize> = (0..inputs.len()).filter(|&i| inputs[i].kind == kind).collect();
            if idx.is_empty() {
                continue;
            }
            let texts: Vec<&str> = idx.iter().map(|&i| inputs[i].text.as_str()).collect();
            for (i, v) in idx.into_iter().zip(self.embed_kind(kind, &texts)?) {
                out[i] = Some(v);
            }
        }
        out.into_iter().map(|v| v.ok_or_else(|| self.client.error("missing vector"))).collect()
    }
}

/// A reranker behind a provider's API (`voyage`, `cohere`).
pub struct RemoteReranker {
    spec: RemoteSpec,
    client: Client,
    key: String,
}

impl RemoteReranker {
    /// A reranker of `spec` calling `endpoint`.
    ///
    /// # Errors
    ///
    /// An embedder spec.
    pub fn new(spec: RemoteSpec, endpoint: Endpoint) -> Result<RemoteReranker> {
        if spec.role != Role::Reranker {
            return Err(Error::Invalid(format!("`{}` is not a reranker preset", spec.name)));
        }
        let p = spec.provider;
        let text = format!("remote\n{}\n{}\n{}", p.as_str(), spec.model, p.api_version());
        let key = blake3::hash(text.as_bytes()).to_hex().to_string();
        let client = Client::new(p, endpoint);
        Ok(RemoteReranker { spec, client, key })
    }
}

impl Reranker for RemoteReranker {
    fn rerank(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let model = self.spec.model.as_str();
        let (path, body, list) = match self.spec.provider {
            Provider::Cohere => ("/v2/rerank", json!({"model": model, "query": query, "documents": docs}), "results"),
            _ => ("/v1/rerank", json!({"model": model, "query": query, "documents": docs, "truncation": true}), "data"),
        };
        let answer = self.client.post(path, &body)?;
        let rows = answer[list].as_array().ok_or_else(|| self.client.error(format!("answer has no `{list}`")))?;
        let mut out: Vec<Option<f32>> = vec![None; docs.len()];
        for r in rows {
            let i = r["index"].as_u64().unwrap_or(u64::MAX);
            #[allow(clippy::cast_possible_truncation)]
            let s = r["relevance_score"].as_f64().map(|x| x as f32);
            if let (Some(slot), Some(s)) = (usize::try_from(i).ok().and_then(|i| out.get_mut(i)), s) {
                *slot = Some(s);
            }
        }
        out.into_iter().map(|s| s.ok_or_else(|| self.client.error("a document has no score"))).collect()
    }

    fn preset(&self) -> &str {
        &self.spec.name
    }

    fn model_key(&self) -> &str {
        &self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names() {
        assert_eq!(RemoteSpec::parse("qwen3-0.6b-q8").unwrap(), None);
        let s = RemoteSpec::parse("remote:voyage/voyage-code-3").unwrap().unwrap();
        assert_eq!((s.provider, s.model.as_str(), s.role), (Provider::Voyage, "voyage-code-3", Role::Embedder));
        let r = RemoteSpec::parse("remote:cohere/rerank-v3.5").unwrap().unwrap();
        assert_eq!(r.role, Role::Reranker);
        assert!(matches!(RemoteSpec::parse("remote:bedrock/titan-embed-text-v2"), Err(Error::Unavailable { .. })));
        assert!(matches!(RemoteSpec::parse("remote:cohere/embed-v4"), Err(Error::Unavailable { .. })));
        assert!(matches!(RemoteSpec::parse("remote:acme/x"), Err(Error::Invalid(_))));
        assert!(matches!(RemoteSpec::parse("remote:voyage"), Err(Error::Invalid(_))));
    }

    #[test]
    fn keys_depend_on_provider_model_and_width() {
        let s = RemoteSpec::parse("remote:voyage/voyage-code-3").unwrap().unwrap();
        let a = describe(&s, &EmbedOptions::default()).unwrap();
        assert_eq!((a.dims, a.backend, a.pooling), (1024, Backend::Remote, Pooling::Provider));
        let b = describe(&s, &EmbedOptions { dims: Some(512), ..EmbedOptions::default() }).unwrap();
        assert_ne!(a.embed_key, b.embed_key);
        let o = RemoteSpec::parse("remote:openai/voyage-code-3").unwrap().unwrap();
        assert_ne!(
            describe(&o, &EmbedOptions { dims: Some(1024), ..EmbedOptions::default() }).unwrap().embed_key,
            a.embed_key
        );
        assert!(describe(&s, &EmbedOptions { dims: Some(4096), ..EmbedOptions::default() }).is_err());
        assert_eq!(describe(&o, &EmbedOptions::default()).unwrap().dims, 1536);
    }

    #[test]
    fn classifies_answers() {
        assert!(matches!(classify(200, None, "{\"a\":1}", "u"), Attempt::Done(_)));
        assert!(matches!(classify(200, None, "nope", "u"), Attempt::Fail(_)));
        assert!(matches!(classify(429, Some(Duration::from_secs(2)), "", "u"), Attempt::Retry(Some(_), _)));
        assert!(matches!(classify(503, None, "", "u"), Attempt::Retry(None, _)));
        assert!(
            matches!(classify(401, None, "{\"detail\":\"bad key\"}", "u"), Attempt::Fail(m) if m.contains("bad key"))
        );
    }
}
