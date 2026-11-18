//! Repository routing (RFC 0006 open question 1): a query also scores the
//! repositories of the organization, and that score weighs the symbol
//! ranking.
//!
//! Two signals, both computed over the **visible** repositories only (so a
//! repository the caller cannot read changes no score):
//!
//! - **Term distribution** (CORI, Callan et al. 1995, the classic
//!   resource-selection score), read off the organization symbol index
//!   itself: each repository is one tantivy segment, so the number of a
//!   repository's symbols whose name, source or path holds a query term is
//!   a dictionary lookup. A repository holding several of the query's rarer
//!   terms beats one holding only the common ones.
//! - **Repository document** ([`RepoProfile`]): repository and package
//!   names, manifest descriptions and the README head, the names of the
//!   most referenced symbols, the path vocabulary, scored with BM25 per
//!   field (weights in [`RouteParams`]).
//!
//! - **Embedding centroid** (RFC 0007 §10), when the query has a vector:
//!   the cosine between it and the repository's [`Centroid`] (the mean of
//!   its symbol vectors, re-normalized, in its profile), for repositories
//!   whose vectors share the query's embed key.
//!
//! Each signal is min-max normalized over the visible repositories and
//! mixed (`mu` = weight of the document); with a centroid signal (at least
//! two visible repositories have one), a repository that has a centroid
//! mixes the three as `(2(1 − mu)·terms + 2mu·doc + w·centroid) / (2 + w)`,
//! `w` = [`RouteParams::centroid`] (1: equal weights at `mu` = 0.5), one
//! without keeps the two-signal mix. The result `r ∈ [0, 1]` is the
//! repository's relevance. [`RouteParams::mode`] decides how it is used:
//! `prior` multiplies every symbol score by `((eps + r) / (eps + 1))^alpha`
//! (the default), `top` searches only the repositories whose `r` is within
//! `ratio` of the best (at most `top`), `off` leaves the ranking alone.

use std::collections::{BTreeSet, HashMap};

use rueca_model::{EdgeKind, FileEntry, Label};
use serde::{Deserialize, Serialize};

use super::link::RepoDeps;
use crate::Segment;
use crate::priors::{is_test_path, is_vendored_path};
use crate::tokenizer::tokens;

/// Version of [`RepoProfile`]; a new version recomputes every profile
/// (2: the embedding centroid).
pub const PROFILE_FORMAT: u32 = 2;

/// Bytes of the README kept in a profile.
const README_BYTES: usize = 4096;
/// Most referenced symbols kept in a profile.
const TOP_SYMBOLS: usize = 64;
/// Path components kept in a profile.
const TOP_PATHS: usize = 64;

/// The routing document of one repository snapshot, plus its package
/// registry entries (stored per store segment under `profiles/`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoProfile {
    pub format: u32,
    /// Version of the linker whose analysis `deps` holds (0: none).
    pub linker: u32,
    pub repo: String,
    /// Repository id parts (owner and name) and published package names.
    pub name: String,
    /// Manifest descriptions and the head of the README.
    pub about: String,
    /// Names of the most referenced symbols (incoming calls, references,
    /// inheritance), most referenced first.
    pub symbols: String,
    /// The most frequent directory names and file stems.
    pub paths: String,
    /// Languages with at least one file.
    pub languages: Vec<String>,
    /// Packages published, imports not resolved inside, packages required.
    #[serde(default)]
    pub deps: RepoDeps,
    /// The mean of the snapshot's symbol vectors, when it has vectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centroid: Option<Centroid>,
}

/// The embedding centroid of a repository snapshot (RFC 0007 §10): the
/// mean of its (dequantized) symbol vectors, re-normalized.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Centroid {
    /// The embed key of the vectors (a query vector must share it).
    pub embed_key: String,
    /// Their model key (embed key, width, encoding).
    pub model_key: String,
    /// The width: the segment's.
    pub dims: usize,
    /// The unit mean vector.
    pub vector: Vec<f32>,
}

impl Centroid {
    /// The centroid of `seg`'s vectors; `None` without vectors (or when
    /// they cannot be read).
    pub fn of(seg: &Segment) -> Option<Centroid> {
        let v = seg.vectors().ok()??;
        if v.is_empty() {
            return None;
        }
        let mut sum = vec![0f64; v.dims()];
        for row in 0..v.len() {
            for (s, x) in sum.iter_mut().zip(v.row(row)) {
                *s += f64::from(x);
            }
        }
        let mean: Vec<f32> = sum.iter().map(|&s| (s / v.len() as f64) as f32).collect();
        let meta = v.meta();
        Some(Centroid {
            embed_key: meta.embed_key.clone(),
            model_key: meta.model_key.clone(),
            dims: v.dims(),
            vector: crate::vectors::truncate_normalize(&mean, v.dims()),
        })
    }

    /// Cosine with a unit query vector of the same embed key (truncated to
    /// the centroid's width and re-normalized).
    pub fn cosine(&self, query: &[f32]) -> f32 {
        let q = crate::vectors::truncate_normalize(query, self.dims);
        q.iter().zip(&self.vector).map(|(a, b)| a * b).sum()
    }
}

fn readme_rank(path: &str) -> Option<(usize, usize, String)> {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let ext = name.strip_prefix("readme")?;
    let ok = ext.is_empty() || matches!(ext, ".md" | ".markdown" | ".rst" | ".txt" | ".adoc" | ".org");
    let pref = if ext == ".md" { 0 } else { 1 };
    ok.then(|| (path.matches('/').count(), pref, path.to_string()))
}

/// The prose of a README: badge, image and HTML lines dropped, URLs
/// removed.
fn clean_readme(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("[![") || t.starts_with("![") || t.starts_with('<') || t.contains("shields.io") {
            continue;
        }
        for w in t.split_whitespace() {
            if w.contains("://") || w.starts_with("](") {
                continue;
            }
            out.push_str(w);
            out.push(' ');
        }
        out.push('\n');
    }
    out
}

/// `"..."` value of `key = "..."` lines (TOML, setup.cfg) in `text`.
fn quoted_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.trim();
        Some(v.trim_matches(|c| c == '"' || c == '\''))
    })
}

/// Descriptions declared by root manifests.
fn descriptions(seg: &Segment) -> Vec<String> {
    let mut out = Vec::new();
    let read = |path: &str| -> Option<String> {
        let f = seg.file_by_path(path)?;
        let bytes = seg.content(f).ok()?;
        String::from_utf8(bytes).ok()
    };
    for json in ["package.json", "composer.json"] {
        if let Some(t) = read(json)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
            && let Some(d) = v.get("description").and_then(|d| d.as_str())
        {
            out.push(d.to_string());
        }
    }
    for (file, key) in [("pyproject.toml", "description"), ("setup.cfg", "description")] {
        if let Some(t) = read(file)
            && let Some(d) = quoted_value(&t, key)
        {
            out.push(d.to_string());
        }
    }
    if let Some(t) = read("pom.xml")
        && let Some(start) = t.find("<description>")
        && let Some(len) = t[start + 13..].find("</description>")
    {
        out.push(t[start + 13..start + 13 + len].to_string());
    }
    out
}

impl RepoProfile {
    /// Builds the profile of `seg`; `deps` comes from the linker.
    pub fn build(seg: &Segment, deps: RepoDeps, linker: u32) -> RepoProfile {
        let repo = seg.snapshot().repo.clone();
        // Name: owner and repository name, then package names.
        let mut name_parts: Vec<String> = repo.split('/').skip(1).map(str::to_string).collect();
        if name_parts.is_empty() {
            name_parts.push(repo.clone());
        }
        for p in &deps.packages {
            name_parts.push(p.name.clone());
        }
        let mut about = descriptions(seg).join("\n");
        if let Some(head) = readme_head(seg) {
            about.push('\n');
            about.push_str(&head);
        }
        let symbols = top_symbols(seg);
        let (paths, languages) = path_vocabulary(seg.files());
        RepoProfile {
            format: PROFILE_FORMAT,
            linker,
            repo,
            name: name_parts.join(" "),
            about,
            symbols: symbols.join(" "),
            paths: paths.join(" "),
            languages: languages.into_iter().collect(),
            deps,
            centroid: Centroid::of(seg),
        }
    }
}

/// The head of the repository's main README, cleaned.
fn readme_head(seg: &Segment) -> Option<String> {
    let (_, f) = seg.files().iter().enumerate().filter_map(|(i, f)| readme_rank(&f.path).map(|r| (r, i))).min()?;
    let bytes = seg.content(f as u32).ok()?;
    let head = &bytes[..bytes.len().min(README_BYTES)];
    Some(clean_readme(&String::from_utf8_lossy(head)))
}

/// Most referenced symbols: by the number of distinct sources of incoming
/// references (at least two), source definitions only.
fn top_symbols(seg: &Segment) -> Vec<&str> {
    let files = seg.files();
    let source_file = |f: Option<u32>| {
        f.and_then(|f| files.get(f as usize))
            .is_some_and(|e| e.language.is_some() && !is_test_path(&e.path) && !is_vendored_path(&e.path))
    };
    let mut counted: Vec<(usize, &str)> = Vec::new();
    for (i, n) in seg.nodes().iter().enumerate() {
        let structural = matches!(
            n.label,
            Label::Repository | Label::Folder | Label::File | Label::Module | Label::Package | Label::Test
        );
        if n.is_test || structural || !source_file(n.file) {
            continue;
        }
        let referrers = distinct_referrers(seg, i as u32);
        if referrers >= 2 {
            counted.push((referrers, n.name.as_str()));
        }
    }
    counted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let mut seen = BTreeSet::new();
    counted.into_iter().map(|(_, n)| n).filter(|n| seen.insert(*n)).take(TOP_SYMBOLS).collect()
}

/// Distinct other nodes that call, reference, inherit or implement node `n`.
fn distinct_referrers(seg: &Segment, n: u32) -> usize {
    let mut srcs: BTreeSet<u32> = BTreeSet::new();
    for kind in [EdgeKind::Calls, EdgeKind::References, EdgeKind::Inherits, EdgeKind::Implements] {
        srcs.extend(seg.in_edges(n, Some(kind)).iter().map(|e| e.other).filter(|&o| o != n));
    }
    srcs.len()
}

/// Path vocabulary — the most frequent directory names and file stems of
/// source files — and the languages of the repository.
fn path_vocabulary(files: &[FileEntry]) -> (Vec<String>, BTreeSet<String>) {
    let mut comp: HashMap<String, usize> = HashMap::new();
    let mut languages = BTreeSet::new();
    for f in files {
        let Some(lang) = f.language else { continue };
        languages.insert(lang.as_str().to_string());
        if is_test_path(&f.path) || is_vendored_path(&f.path) {
            continue;
        }
        let mut parts: Vec<&str> = f.path.split('/').collect();
        if let Some(file) = parts.pop() {
            parts.push(file.split('.').next().unwrap_or(file));
        }
        for p in parts.into_iter().filter(|p| !p.is_empty()) {
            *comp.entry(p.to_lowercase()).or_default() += 1;
        }
    }
    let mut comp: Vec<(String, usize)> = comp.into_iter().collect();
    comp.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    (comp.into_iter().take(TOP_PATHS).map(|(p, _)| p).collect(), languages)
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// How the repository relevance weighs a ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMode {
    /// No routing: one ranking with organization-wide BM25 only.
    Off,
    /// Multiply every symbol score by the repository's prior.
    Prior,
    /// Search only the best repositories.
    Top,
}

/// Routing parameters. The defaults were tuned on the benchmark's
/// calibration split (`bench/README.md`, *Org scenarios*); `RUECA_ORG_ROUTE`
/// overrides them for experiments (`off`, `prior,alpha=2,eps=0.1,mu=0.5`,
/// `top,top=3,ratio=0.5`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteParams {
    pub mode: RouteMode,
    /// Exponent of the prior.
    pub alpha: f32,
    /// Floor of the prior: a repository with relevance 0 keeps
    /// `(eps / (eps + 1))^alpha` of its scores.
    pub eps: f32,
    /// Weight of the repository document against the term distribution.
    pub mu: f32,
    /// `top` mode: most repositories searched.
    pub top: usize,
    /// `top` mode: repositories with relevance ≥ `ratio` × the best's.
    pub ratio: f32,
    /// BM25 weights of the profile fields: name, about, symbols, paths.
    pub fields: [f32; 4],
    /// CORI term-frequency constants: `T = df / (df + k + c · cw / avg_cw)`
    /// (Callan et al.: 50 and 150).
    pub cori_k: f32,
    pub cori_c: f32,
    /// Score each repository with its own BM25 statistics (its ranking is
    /// then the one it has alone) instead of organization-wide ones.
    pub local_stats: bool,
    /// Weight of the embedding-centroid signal (0: off; 1: equal to the
    /// two others at `mu` = 0.5).
    pub centroid: f32,
}

impl Default for RouteParams {
    fn default() -> RouteParams {
        RouteParams {
            mode: RouteMode::Prior,
            alpha: 2.0,
            eps: 0.25,
            mu: 0.5,
            top: 3,
            ratio: 0.5,
            fields: [3.0, 1.0, 1.0, 0.5],
            cori_k: 50.0,
            cori_c: 150.0,
            local_stats: false,
            centroid: 1.0,
        }
    }
}

impl RouteParams {
    /// Parses `mode[,key=value...]` (see the type docs); unknown keys and
    /// bad values are errors.
    pub fn parse(spec: &str) -> std::result::Result<RouteParams, String> {
        let mut p = RouteParams::default();
        for (i, part) in spec.split(',').map(str::trim).filter(|s| !s.is_empty()).enumerate() {
            if i == 0 && !part.contains('=') {
                p.mode = match part {
                    "off" => RouteMode::Off,
                    "prior" => RouteMode::Prior,
                    "top" => RouteMode::Top,
                    m => return Err(format!("unknown routing mode `{m}` (off, prior, top)")),
                };
                continue;
            }
            let (k, v) = part.split_once('=').ok_or_else(|| format!("expected key=value, got `{part}`"))?;
            let f = || v.trim().parse::<f32>().map_err(|_| format!("bad number `{v}` for `{k}`"));
            match k.trim() {
                "alpha" => p.alpha = f()?,
                "eps" => p.eps = f()?,
                "mu" => p.mu = f()?,
                "ratio" => p.ratio = f()?,
                "top" => p.top = f()? as usize,
                "name" => p.fields[0] = f()?,
                "about" => p.fields[1] = f()?,
                "symbols" => p.fields[2] = f()?,
                "paths" => p.fields[3] = f()?,
                "cori_k" => p.cori_k = f()?,
                "cori_c" => p.cori_c = f()?,
                "centroid" => p.centroid = f()?,
                "stats" => {
                    p.local_stats = match v.trim() {
                        "local" => true,
                        "org" => false,
                        other => return Err(format!("`stats` is `org` or `local`, got `{other}`")),
                    }
                }
                other => return Err(format!("unknown routing parameter `{other}`")),
            }
        }
        Ok(p)
    }

    /// The defaults, or `RUECA_ORG_ROUTE` when set and valid; the centroid
    /// weight from `RUECA_ORG_ROUTE_CENTROID` when set.
    pub fn from_env() -> RouteParams {
        RouteParams::resolve(None)
    }

    /// [`RouteParams::from_env`] with the centroid weight of the
    /// configuration (`[retrieval] route_centroid`) under the environment.
    pub fn resolve(config_centroid: Option<f32>) -> RouteParams {
        let mut p: RouteParams =
            std::env::var("RUECA_ORG_ROUTE").ok().and_then(|s| RouteParams::parse(&s).ok()).unwrap_or_default();
        let env = std::env::var("RUECA_ORG_ROUTE_CENTROID").ok().and_then(|v| v.trim().parse::<f32>().ok());
        if let Some(w) = env.or(config_centroid).filter(|w| w.is_finite() && *w >= 0.0) {
            p.centroid = w;
        }
        p
    }

    /// The relevance of a repository from its normalized signals (see the
    /// module docs).
    pub fn mix(&self, terms: f32, doc: f32, centroid: Option<f32>) -> f32 {
        let two = (1.0 - self.mu) * terms + self.mu * doc;
        match centroid {
            Some(c) if self.centroid > 0.0 => (2.0 * two + self.centroid * c) / (2.0 + self.centroid),
            _ => two,
        }
    }

    /// The prior of a repository with relevance `r`.
    pub fn prior(&self, r: f32) -> f32 {
        if self.alpha == 0.0 {
            return 1.0;
        }
        ((self.eps + r.clamp(0.0, 1.0)) / (self.eps + 1.0)).powf(self.alpha)
    }
}

// ---------------------------------------------------------------------------
// The repository documents of a generation
// ---------------------------------------------------------------------------

/// One profile field of a BM25 scoring: its index and weight, and the
/// number of repositories with a profile and the field's average length.
#[derive(Clone, Copy)]
struct FieldScoring {
    k: usize,
    weight: f32,
    n: f32,
    avg: f32,
}

/// Term frequencies of each profile field of each repository.
#[derive(Debug, Default)]
pub(crate) struct RepoDocs {
    /// `[repo][field]` → term → frequency.
    tf: Vec<Option<[HashMap<String, u32>; 4]>>,
    /// `[repo][field]` → field length in tokens.
    len: Vec<[u32; 4]>,
}

impl RepoDocs {
    pub(crate) fn build(profiles: &[Option<RepoProfile>]) -> RepoDocs {
        let mut docs = RepoDocs::default();
        for p in profiles {
            match p {
                Some(p) => {
                    let mut tf: [HashMap<String, u32>; 4] = Default::default();
                    let mut len = [0u32; 4];
                    for (k, text) in [&p.name, &p.about, &p.symbols, &p.paths].into_iter().enumerate() {
                        for t in tokens(text) {
                            *tf[k].entry(t).or_default() += 1;
                            len[k] += 1;
                        }
                    }
                    docs.tf.push(Some(tf));
                    docs.len.push(len);
                }
                None => {
                    docs.tf.push(None);
                    docs.len.push([0; 4]);
                }
            }
        }
        docs
    }

    /// BM25 of `terms` against each of `repos` (field-weighted sum).
    pub(crate) fn scores(&self, terms: &[String], repos: &[usize], weights: &[f32; 4]) -> Vec<f32> {
        let n = repos.iter().filter(|&&r| self.tf.get(r).is_some_and(Option::is_some)).count() as f32;
        let mut out = vec![0f32; repos.len()];
        if n == 0.0 {
            return out;
        }
        for k in (0..4).filter(|&k| weights[k] != 0.0) {
            let avg = repos.iter().map(|&r| self.len[r][k] as f32).sum::<f32>() / n;
            let field = FieldScoring { k, weight: weights[k], n, avg };
            for t in terms {
                self.add_term_scores(t, &field, repos, &mut out);
            }
        }
        out
    }

    /// Adds the weighted BM25 of term `t` in one field to each repository's
    /// score.
    fn add_term_scores(&self, t: &str, field: &FieldScoring, repos: &[usize], out: &mut [f32]) {
        const K1: f32 = 1.2;
        const B: f32 = 0.75;
        let FieldScoring { k, weight, n, avg } = *field;
        let df = repos.iter().filter(|&&r| self.tf[r].as_ref().is_some_and(|f| f[k].contains_key(t))).count() as f32;
        if df == 0.0 {
            return;
        }
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        for (j, &r) in repos.iter().enumerate() {
            let Some(f) = &self.tf[r] else { continue };
            let Some(&tf) = f[k].get(t) else { continue };
            let tf = tf as f32;
            let norm = if avg > 0.0 { self.len[r][k] as f32 / avg } else { 1.0 };
            out[j] += weight * idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * norm));
        }
    }
}

/// CORI belief of each repository for `terms`, from `df[term][repo]` (the
/// repository's documents holding the term) and `cw[repo]` (its documents).
pub(crate) fn cori(df: &[Vec<u64>], cw: &[u64], k: f32, c_len: f32) -> Vec<f32> {
    let c = cw.len();
    let mut out = vec![0f32; c];
    if c == 0 {
        return out;
    }
    let avg = cw.iter().sum::<u64>() as f32 / c as f32;
    let mut used = 0usize;
    for row in df {
        let cf = row.iter().filter(|&&d| d > 0).count();
        if cf == 0 {
            continue;
        }
        used += 1;
        let i = ((c as f32 + 0.5) / cf as f32).ln() / (c as f32 + 1.0).ln();
        for (r, &d) in row.iter().enumerate() {
            let d = d as f32;
            let t = if d > 0.0 { d / (d + k + c_len * cw[r] as f32 / avg.max(1.0)) } else { 0.0 };
            out[r] += 0.4 + 0.6 * t * i;
        }
    }
    if used > 0 {
        for x in &mut out {
            *x /= used as f32;
        }
    }
    out
}

/// Min-max normalization to [0, 1] (all zeros when constant).
pub(crate) fn normalize(v: &[f32]) -> Vec<f32> {
    let (lo, hi) = v.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        return vec![0.0; v.len()];
    }
    v.iter().map(|&x| (x - lo) / (hi - lo)).collect()
}

/// Distinct query terms (code tokenizer), in first-occurrence order.
pub(crate) fn query_terms(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    tokens(text).into_iter().filter(|t| seen.insert(t.clone())).collect()
}

/// The relevance of one repository for a query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepoRoute {
    /// Index into the generation's repositories.
    pub repo: usize,
    /// Relevance in [0, 1] (1: the best visible repository).
    pub score: f32,
    /// The signals, normalized: term distribution, document and (with a
    /// query vector and a centroid) embedding centroid.
    pub terms: f32,
    pub doc: f32,
    pub centroid: Option<f32>,
}

/// A query's vector, for the centroid signal.
#[derive(Debug, Clone, Copy)]
pub struct RouteQuery<'a> {
    /// The embed key it was embedded with.
    pub embed_key: &'a str,
    /// The unit vector (full width; truncated per centroid).
    pub vector: &'a [f32],
}

/// The normalized centroid signal of each repository (`None`: no centroid
/// for the query's model); all `None` unless at least two have one.
pub(crate) fn centroid_signal(profiles: &[Option<&RepoProfile>], q: &RouteQuery<'_>) -> Vec<Option<f32>> {
    let raw: Vec<Option<f32>> = profiles
        .iter()
        .map(|p| p.and_then(|p| p.centroid.as_ref()).filter(|c| c.embed_key == q.embed_key).map(|c| c.cosine(q.vector)))
        .collect();
    let present: Vec<f32> = raw.iter().flatten().copied().collect();
    if present.len() < 2 {
        return vec![None; raw.len()];
    }
    let norm = normalize(&present);
    let mut it = norm.into_iter();
    raw.iter().map(|x| x.and_then(|_| it.next())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_parse() {
        assert_eq!(RouteParams::parse("off").unwrap().mode, RouteMode::Off);
        let p = RouteParams::parse("top,top=2,ratio=0.7,mu=0.25").unwrap();
        assert_eq!((p.mode, p.top, p.ratio, p.mu), (RouteMode::Top, 2, 0.7, 0.25));
        assert!(RouteParams::parse("prior,zeta=1").is_err());
        assert!(RouteParams::parse("fast").is_err());
        let p = RouteParams::parse("prior,alpha=2,eps=0").unwrap();
        assert_eq!(p.prior(1.0), 1.0);
        assert_eq!(p.prior(0.5), 0.25);
        assert_eq!(RouteParams { alpha: 0.0, ..p }.prior(0.0), 1.0);
        assert_eq!(RouteParams::parse("prior,centroid=0.5").unwrap().centroid, 0.5);
    }

    #[test]
    fn mixing_with_and_without_a_centroid() {
        let p = RouteParams::default();
        assert_eq!(p.mix(1.0, 0.0, None), 0.5);
        // Equal weights: (1 + 0 + 1) / 3.
        assert!((p.mix(1.0, 0.0, Some(1.0)) - 2.0 / 3.0).abs() < 1e-6);
        let off = RouteParams { centroid: 0.0, ..p };
        assert_eq!(off.mix(1.0, 0.0, Some(1.0)), 0.5);
        let c = |key: &str, v: Vec<f32>| RepoProfile {
            centroid: Some(Centroid { embed_key: key.into(), model_key: String::new(), dims: v.len(), vector: v }),
            ..RepoProfile::default()
        };
        let (a, b, x) = (c("k", vec![1.0, 0.0]), c("k", vec![0.0, 1.0]), c("other", vec![1.0, 0.0]));
        let q = RouteQuery { embed_key: "k", vector: &[1.0, 0.0] };
        assert_eq!(centroid_signal(&[Some(&a), Some(&b), Some(&x), None], &q), [Some(1.0), Some(0.0), None, None]);
        assert_eq!(centroid_signal(&[Some(&a), Some(&x)], &q), [None, None], "one centroid is no signal");
    }

    #[test]
    fn cori_prefers_rare_terms_held_together() {
        // Term 0 is everywhere, term 1 only in repository 2.
        let df = vec![vec![10, 10, 10], vec![0, 0, 3]];
        let s = cori(&df, &[100, 100, 100], 50.0, 150.0);
        assert!(s[2] > s[0] && (s[0] - s[1]).abs() < 1e-6, "{s:?}");
        assert_eq!(normalize(&[1.0, 3.0, 2.0]), vec![0.0, 1.0, 0.5]);
        assert_eq!(normalize(&[2.0, 2.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn readme_and_values() {
        assert!(readme_rank("README.md").unwrap() < readme_rank("docs/README.md").unwrap());
        assert!(readme_rank("README.md").unwrap() < readme_rank("README.rst").unwrap());
        assert!(readme_rank("src/readme_test.py").is_none());
        let c =
            clean_readme("# jq\n[![ci](https://x/badge.svg)](https://x)\njq is a `sed` for JSON https://jqlang.org\n");
        assert!(c.contains("jq is a `sed` for JSON") && !c.contains("https"), "{c}");
        assert_eq!(
            quoted_value("[project]\nname = \"click\"\ndescription = 'CLI kit'\n", "description"),
            Some("CLI kit")
        );
    }
}
