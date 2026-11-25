//! `execute`'s `budget_tokens`: the result, as compact JSON, counted with
//! the tools' estimator ([`rueca_query::compact::count_tokens`],
//! `ceil(bytes / 4)`), is cut until it fits:
//!
//! 1. the largest array in it (by serialized size; the outermost one when it
//!    holds the rest) keeps the longest prefix that fits, or becomes empty
//!    and the next largest is cut, and so on;
//! 2. with no array left to cut, the longest string keeps the longest
//!    prefix that fits, followed by `…`;
//! 3. a result that still does not fit (an object with very many keys)
//!    becomes `null`.
//!
//! Each step is a binary search over one array or string, so cutting costs
//! a few serializations per array, not one per element.

use rueca_query::compact::count_tokens;
use serde_json::Value;

/// One step of a path into a JSON value.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Key(String),
    Index(usize),
}

fn tokens(v: &Value) -> usize {
    count_tokens(&v.to_string())
}

/// Cuts `result` to `budget` tokens; whether anything was cut.
pub(crate) fn fit(result: &mut Value, budget: usize) -> bool {
    if tokens(result) <= budget {
        return false;
    }
    while tokens(result) > budget {
        if let Some(path) = largest(result, &|v| matches!(v, Value::Array(a) if !a.is_empty())) {
            cut_array(result, &path, budget);
        } else if let Some(path) = largest(result, &|v| matches!(v, Value::String(s) if !s.is_empty())) {
            cut_string(result, &path, budget);
        } else {
            *result = Value::Null;
        }
    }
    true
}

/// The path of the largest value (by serialized size) matching `pick`,
/// the outermost on ties.
fn largest(v: &Value, pick: &dyn Fn(&Value) -> bool) -> Option<Vec<Step>> {
    let mut best: Option<(usize, Vec<Step>)> = None;
    let mut path = Vec::new();
    walk(v, &mut path, &mut |p, x| {
        if pick(x) {
            let size = x.to_string().len();
            if best.as_ref().is_none_or(|(b, _)| size > *b) {
                best = Some((size, p.to_vec()));
            }
        }
    });
    best.map(|(_, p)| p)
}

fn walk(v: &Value, path: &mut Vec<Step>, visit: &mut dyn FnMut(&[Step], &Value)) {
    visit(path, v);
    match v {
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(Step::Index(i));
                walk(x, path, visit);
                path.pop();
            }
        }
        Value::Object(m) => {
            for (k, x) in m {
                path.push(Step::Key(k.clone()));
                walk(x, path, visit);
                path.pop();
            }
        }
        _ => {}
    }
}

fn at<'a>(v: &'a mut Value, path: &[Step]) -> &'a mut Value {
    path.iter().fold(v, |v, step| match step {
        Step::Key(k) => &mut v[k.as_str()],
        Step::Index(i) => &mut v[*i],
    })
}

/// Keeps the longest prefix of the array at `path` with which `root` fits
/// (none: the array becomes empty).
fn cut_array(root: &mut Value, path: &[Step], budget: usize) {
    let full = match at(root, path) {
        Value::Array(a) => std::mem::take(a),
        _ => return,
    };
    let keep = longest_fit(full.len(), |k| {
        *at(root, path) = Value::Array(full[..k].to_vec());
        tokens(root) <= budget
    });
    *at(root, path) = Value::Array(full[..keep].to_vec());
}

/// Keeps the longest prefix of the string at `path` (plus `…`) with which
/// `root` fits (none: the string becomes empty).
fn cut_string(root: &mut Value, path: &[Step], budget: usize) {
    let full = match at(root, path) {
        Value::String(s) => std::mem::take(s),
        _ => return,
    };
    let chars: Vec<char> = full.chars().collect();
    let prefix = |k: usize| -> String {
        let mut s: String = chars[..k].iter().collect();
        if k > 0 {
            s.push('…');
        }
        s
    };
    let keep = longest_fit(chars.len(), |k| {
        *at(root, path) = Value::String(prefix(k));
        tokens(root) <= budget
    });
    *at(root, path) = Value::String(prefix(keep));
}

/// The largest `k < n` for which `fits(k)` (0 when none does); `fits` is
/// monotone (a shorter prefix never fits worse).
fn longest_fit(n: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n.saturating_sub(1));
    let mut best = 0;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            best = mid;
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fitting_results_are_untouched() {
        let mut v = json!({"a": [1, 2, 3]});
        assert!(!fit(&mut v, 100));
        assert_eq!(v, json!({"a": [1, 2, 3]}));
    }

    #[test]
    fn arrays_are_cut_from_the_end() {
        let mut v = json!((0..100).map(|i| format!("item-{i:03}")).collect::<Vec<_>>());
        assert!(fit(&mut v, 30));
        let kept = v.as_array().unwrap();
        assert!(!kept.is_empty() && kept.len() < 100);
        assert_eq!(kept[0], "item-000");
        assert!(tokens(&v) <= 30);
        // One more element would not fit.
        let mut more = kept.clone();
        more.push(json!(format!("item-{:03}", kept.len())));
        assert!(tokens(&Value::Array(more)) > 30);
    }

    #[test]
    fn the_largest_array_goes_first() {
        let big: Vec<String> = (0..200).map(|i| format!("x{i}")).collect();
        let mut v = json!({"small": [1, 2], "big": big, "n": 7});
        assert!(fit(&mut v, 40));
        assert_eq!(v["small"], json!([1, 2]));
        assert_eq!(v["n"], 7);
        assert!(tokens(&v) <= 40);
    }

    #[test]
    fn strings_then_null() {
        let mut v = json!({"text": "y".repeat(4000)});
        assert!(fit(&mut v, 100));
        let s = v["text"].as_str().unwrap();
        assert!(s.ends_with('…') && s.len() > 300, "{}", s.len());
        assert!(tokens(&v) <= 100);
        let keys: serde_json::Map<String, Value> = (0..500).map(|i| (format!("key{i}"), json!(i))).collect();
        let mut v = Value::Object(keys);
        assert!(fit(&mut v, 100));
        assert_eq!(v, Value::Null);
    }
}
