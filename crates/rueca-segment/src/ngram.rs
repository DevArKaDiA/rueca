//! Trigram content index and regex query planning.
//!
//! Index: trigram of ASCII-lowercased bytes → roaring bitmap of file ids.
//! Planning: a walk over the `regex_syntax` HIR computes, per sub-expression,
//! either a small exact set of (lowercased) strings it can match or a boolean
//! trigram requirement. Concatenations take cross products of exact sets
//! (bounded), alternations take unions/ORs, optional repetitions contribute
//! nothing. The final requirement is an AND/OR tree of trigrams; `All` means
//! "no usable literal" (fallback scan).

use std::collections::{BTreeSet, HashMap};

use regex_syntax::hir::{Class, Hir, HirKind};
use roaring::RoaringBitmap;

use crate::{Result, SegmentError};

/// Files larger than this are not indexed for content search.
pub const MAX_TEXT_FILE_BYTES: usize = 2 * 1024 * 1024;
/// Window inspected for a NUL byte to detect binaries.
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;

const MAX_SET: usize = 64;
const MAX_CLASS: u32 = 8;
const MAX_LITERAL: usize = 256;

pub fn is_text(content: &[u8]) -> bool {
    content.len() <= MAX_TEXT_FILE_BYTES && !content[..content.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

#[inline]
fn key(a: u8, b: u8, c: u8) -> u32 {
    ((a.to_ascii_lowercase() as u32) << 16) | ((b.to_ascii_lowercase() as u32) << 8) | c.to_ascii_lowercase() as u32
}

/// Sorted, deduplicated trigram keys of `content`.
pub fn trigrams(content: &[u8]) -> Vec<u32> {
    let mut out: Vec<u32> = content.windows(3).map(|w| key(w[0], w[1], w[2])).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Boolean trigram requirement.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// No constraint (every text file is a candidate).
    All,
    /// Cannot match anything.
    Nothing,
    Tri(u32),
    And(Vec<Plan>),
    Or(Vec<Plan>),
}

impl Plan {
    fn and(parts: Vec<Plan>) -> Plan {
        let mut out = Vec::new();
        for p in parts {
            match p {
                Plan::All => {}
                Plan::Nothing => return Plan::Nothing,
                Plan::And(v) => out.extend(v),
                p => out.push(p),
            }
        }
        out.dedup();
        match out.len() {
            0 => Plan::All,
            1 => out.pop().unwrap(),
            _ => Plan::And(out),
        }
    }

    fn or(parts: Vec<Plan>) -> Plan {
        let mut out = Vec::new();
        for p in parts {
            match p {
                Plan::All => return Plan::All,
                Plan::Nothing => {}
                Plan::Or(v) => out.extend(v),
                p => out.push(p),
            }
        }
        match out.len() {
            0 => Plan::Nothing,
            1 => out.pop().unwrap(),
            _ => Plan::Or(out),
        }
    }
}

type Set = BTreeSet<Vec<u8>>;

struct Info {
    /// The node matches exactly one of these (lowercased) strings.
    exact: Option<Set>,
    /// Additional requirement (beyond `exact`).
    req: Plan,
}

fn lower(b: &[u8]) -> Vec<u8> {
    b.iter().map(|c| c.to_ascii_lowercase()).collect()
}

fn set_plan(set: &Set) -> Plan {
    Plan::or(
        set.iter()
            .map(|s| {
                if s.len() < 3 {
                    Plan::All
                } else {
                    Plan::and(s.windows(3).map(|w| Plan::Tri(key(w[0], w[1], w[2]))).collect())
                }
            })
            .collect(),
    )
}

fn full(i: Info) -> Plan {
    match i.exact {
        Some(s) => Plan::and(vec![i.req, set_plan(&s)]),
        None => i.req,
    }
}

fn exact(set: Set) -> Info {
    Info { exact: Some(set), req: Plan::All }
}

fn empty_string() -> Info {
    exact(std::iter::once(Vec::new()).collect())
}

fn analyze(h: &Hir) -> Info {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => empty_string(),
        HirKind::Literal(lit) => {
            if lit.0.len() > MAX_LITERAL {
                // Keep the plan small: long literals become an AND of their trigrams.
                Info { exact: None, req: set_plan(&std::iter::once(lower(&lit.0)).collect()) }
            } else {
                exact(std::iter::once(lower(&lit.0)).collect())
            }
        }
        HirKind::Class(Class::Unicode(c)) => {
            let n: u32 = c.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if n == 0 {
                return Info { exact: None, req: Plan::Nothing };
            }
            if n > MAX_CLASS {
                return Info { exact: None, req: Plan::All };
            }
            let mut set = Set::new();
            for r in c.ranges() {
                for cp in r.start() as u32..=r.end() as u32 {
                    if let Some(ch) = char::from_u32(cp) {
                        let mut buf = [0u8; 4];
                        set.insert(lower(ch.encode_utf8(&mut buf).as_bytes()));
                    }
                }
            }
            exact(set)
        }
        HirKind::Class(Class::Bytes(c)) => {
            let n: u32 = c.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if n == 0 {
                return Info { exact: None, req: Plan::Nothing };
            }
            if n > MAX_CLASS {
                return Info { exact: None, req: Plan::All };
            }
            let set =
                c.ranges().iter().flat_map(|r| r.start()..=r.end()).map(|b| vec![b.to_ascii_lowercase()]).collect();
            exact(set)
        }
        HirKind::Capture(cap) => analyze(&cap.sub),
        HirKind::Repetition(rep) => {
            if rep.max == Some(0) {
                return empty_string();
            }
            let sub = analyze(&rep.sub);
            if rep.min == 0 {
                Info { exact: None, req: Plan::All }
            } else if rep.min == 1 && rep.max == Some(1) {
                sub
            } else {
                Info { exact: None, req: full(sub) }
            }
        }
        HirKind::Concat(children) => {
            let mut reqs = Vec::new();
            let mut run: Option<Set> = Some(std::iter::once(Vec::new()).collect());
            let mut whole_exact = true;
            for c in children {
                let ci = analyze(c);
                reqs.push(ci.req);
                match (run.take(), ci.exact) {
                    (Some(r), Some(e)) => {
                        if r.len() * e.len() <= MAX_SET {
                            let mut prod = Set::new();
                            for a in &r {
                                for b in &e {
                                    let mut s = a.clone();
                                    s.extend_from_slice(b);
                                    prod.insert(s);
                                }
                            }
                            run = Some(prod);
                        } else {
                            reqs.push(set_plan(&r));
                            run = Some(e);
                            whole_exact = false;
                        }
                    }
                    (Some(r), None) => {
                        reqs.push(set_plan(&r));
                        whole_exact = false;
                    }
                    (None, Some(e)) => run = Some(e),
                    (None, None) => {}
                }
            }
            if whole_exact {
                Info { exact: run, req: Plan::and(reqs) }
            } else {
                if let Some(r) = run {
                    reqs.push(set_plan(&r));
                }
                Info { exact: None, req: Plan::and(reqs) }
            }
        }
        HirKind::Alternation(children) => {
            let infos: Vec<Info> = children.iter().map(analyze).collect();
            let all_exact = infos.iter().all(|i| i.exact.is_some() && i.req == Plan::All);
            if all_exact {
                let mut union = Set::new();
                for i in &infos {
                    union.extend(i.exact.iter().flatten().cloned());
                }
                if union.len() <= MAX_SET {
                    return exact(union);
                }
            }
            Info { exact: None, req: Plan::or(infos.into_iter().map(full).collect()) }
        }
    }
}

/// Parses `pattern` (already a regex; literal callers escape first) into a plan.
pub fn plan(pattern: &str, case_insensitive: bool) -> Result<Plan> {
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(case_insensitive)
        .multi_line(true)
        .build()
        .parse(pattern)
        .map_err(|e| SegmentError::InvalidQuery(e.to_string()))?;
    Ok(full(analyze(&hir)))
}

/// Read-only view of the trigram components.
pub struct NgramIndex<'a> {
    pub keys: &'a [u32],
    pub offsets: &'a [u64],
    pub postings: &'a [u8],
}

impl NgramIndex<'_> {
    fn posting(&self, t: u32) -> Result<RoaringBitmap> {
        match self.keys.binary_search(&t) {
            Ok(i) => {
                let (a, b) = (self.offsets[i] as usize, self.offsets[i + 1] as usize);
                let bytes =
                    self.postings.get(a..b).ok_or_else(|| SegmentError::Corrupt("ngram posting bounds".into()))?;
                RoaringBitmap::deserialize_from(bytes).map_err(|e| SegmentError::Corrupt(format!("ngram posting: {e}")))
            }
            Err(_) => Ok(RoaringBitmap::new()),
        }
    }

    /// Evaluates `plan`; `all` is the universe (every text file).
    pub fn eval(&self, plan: &Plan, all: &RoaringBitmap) -> Result<RoaringBitmap> {
        Ok(match plan {
            Plan::All => all.clone(),
            Plan::Nothing => RoaringBitmap::new(),
            Plan::Tri(t) => self.posting(*t)?,
            Plan::And(v) => {
                let mut acc: Option<RoaringBitmap> = None;
                for p in v {
                    let b = self.eval(p, all)?;
                    acc = Some(match acc {
                        None => b,
                        Some(a) => a & b,
                    });
                    if acc.as_ref().is_some_and(|a| a.is_empty()) {
                        break;
                    }
                }
                acc.unwrap_or_else(|| all.clone())
            }
            Plan::Or(v) => {
                let mut acc = RoaringBitmap::new();
                for p in v {
                    acc |= self.eval(p, all)?;
                }
                acc
            }
        })
    }
}

/// Serialized trigram components: `(keys, offsets, postings)`.
pub fn serialize_postings(map: HashMap<u32, RoaringBitmap>) -> Result<(Vec<u32>, Vec<u64>, Vec<u8>)> {
    let mut keys: Vec<u32> = map.keys().copied().collect();
    keys.sort_unstable();
    let mut offsets = Vec::with_capacity(keys.len() + 1);
    let mut postings = Vec::new();
    offsets.push(0u64);
    for k in &keys {
        let mut bm = map[k].clone();
        bm.optimize();
        bm.serialize_into(&mut postings)?;
        offsets.push(postings.len() as u64);
    }
    Ok((keys, offsets, postings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Plan {
        let b = s.as_bytes();
        Plan::Tri(key(b[0], b[1], b[2]))
    }

    #[test]
    fn literal_plans() {
        assert_eq!(plan("abcd", false).unwrap(), Plan::And(vec![t("abc"), t("bcd")]));
        assert_eq!(plan("ab", false).unwrap(), Plan::All);
        assert_eq!(plan("(?i)ABC", false).unwrap(), t("abc"));
        assert_eq!(plan("ABC", true).unwrap(), t("abc"));
    }

    #[test]
    fn regex_plans() {
        assert_eq!(plan("foo|bar", false).unwrap(), Plan::Or(vec![t("bar"), t("foo")]));
        assert_eq!(plan("foo.*bar", false).unwrap(), Plan::And(vec![t("foo"), t("bar")]));
        assert_eq!(plan(r"\w+", false).unwrap(), Plan::All);
        assert_eq!(plan("x?abc", false).unwrap(), t("abc"));
        assert_eq!(
            plan("(get|set)User", false).unwrap(),
            Plan::Or(vec![
                Plan::And(vec![t("get"), t("etu"), t("tus"), t("use"), t("ser")]),
                Plan::And(vec![t("set"), t("etu"), t("tus"), t("use"), t("ser")]),
            ])
        );
        assert!(plan("(", false).is_err());
    }
}
