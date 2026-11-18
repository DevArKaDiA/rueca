//! Read evidence for idle expiry (RFC 0008 §5.8):
//! `reads/<yyyy-mm-dd>/<hh>-<replica>.json`, one `Create` per replica and
//! hour listing the `(repo, ref)` pairs it served.
//!
//! A query replica records pairs with a [`ReadsRecorder`] and writes the
//! previous hour's object once the hour turns (and at shutdown). The
//! updater ([`last_reads`]) lists the days of the idle window and keeps,
//! per pair, the end of the newest hour that read it. A pair whose repository
//! is `*` was read without a `repo` argument: it counts for that ref of
//! every repository. Only refs that an argument names are recorded (a call
//! without `ref` reads defaults, which never expire).

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use bytes::Bytes;
use rueca_storage::{PutError, Storage};
use serde::{Deserialize, Serialize};

/// Where read objects live.
pub const READS_PREFIX: &str = "reads/";
/// Version of a read object.
pub const READS_FORMAT: u32 = 1;
/// A repository wildcard: the ref was read in every selected repository.
pub const ANY_REPO: &str = "*";

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

/// `reads/<yyyy-mm-dd>/<hh>-<replica>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadsObject {
    pub format: u32,
    pub replica: String,
    /// Start of the hour (unix seconds).
    pub hour: u64,
    /// `(repo, ref)` pairs served in that hour.
    pub refs: Vec<(String, String)>,
}

/// `(year, month, day)` of a unix day number (Howard Hinnant's
/// `civil_from_days`).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

/// `reads/<yyyy-mm-dd>/` of the day holding `t`.
fn day_prefix(t: u64) -> String {
    let (y, m, d) = civil(t / DAY);
    format!("{READS_PREFIX}{y:04}-{m:02}-{d:02}/")
}

/// The object of `replica` for the hour starting at `hour`.
pub fn reads_path(hour: u64, replica: &str) -> String {
    format!("{}{:02}-{replica}.json", day_prefix(hour), (hour % DAY) / HOUR)
}

/// Writes one hour's read pairs (`Create`; an existing object for that
/// replica and hour is left as it is).
pub fn write_reads(
    storage: &dyn Storage,
    replica: &str,
    hour: u64,
    pairs: &BTreeSet<(String, String)>,
) -> anyhow::Result<()> {
    let obj =
        ReadsObject { format: READS_FORMAT, replica: replica.to_string(), hour, refs: pairs.iter().cloned().collect() };
    let path = reads_path(hour, replica);
    match storage.put_new(&path, Bytes::from(serde_json::to_vec(&obj)?)) {
        Ok(_) | Err(PutError::AlreadyExists) => Ok(()),
        Err(e) => Err(anyhow::Error::from(e).context(format!("writing {path}"))),
    }
}

/// Every pair read since `since` (unix seconds) → the end of the newest
/// hour that read it. Lists one `reads/<day>/` prefix per day of the
/// window; unreadable objects are skipped.
pub fn last_reads(storage: &dyn Storage, since: u64, now: u64) -> anyhow::Result<HashMap<(String, String), u64>> {
    let mut out: HashMap<(String, String), u64> = HashMap::new();
    let mut day = (since / DAY) * DAY;
    while day <= now {
        for meta in storage.list(&day_prefix(day))? {
            let Some(v) = storage.get(&meta.path)? else { continue };
            let Ok(obj) = serde_json::from_slice::<ReadsObject>(&v.value) else { continue };
            let end = obj.hour + HOUR;
            if end <= since {
                continue;
            }
            for pair in obj.refs {
                let e = out.entry(pair).or_insert(0);
                *e = (*e).max(end);
            }
        }
        day += DAY;
    }
    Ok(out)
}

/// Deletes read objects of days that ended before `before` (the idle
/// window has passed them); returns how many.
pub fn prune_reads(storage: &dyn Storage, before: u64) -> anyhow::Result<usize> {
    let keep_from = day_prefix(before);
    let mut deleted = 0;
    for meta in storage.list(READS_PREFIX)? {
        // `reads/<yyyy-mm-dd>/…` sorts by day.
        if meta.path.as_str() < keep_from.as_str() {
            storage.delete(&meta.path)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// When `(repo, ref)` was last read according to `reads` (the newest of the
/// exact pair and the `*` pair).
pub fn last_read(reads: &HashMap<(String, String), u64>, repo: &str, ref_name: &str) -> Option<u64> {
    let exact = reads.get(&(repo.to_string(), ref_name.to_string())).copied();
    let any = reads.get(&(ANY_REPO.to_string(), ref_name.to_string())).copied();
    exact.max(any)
}

/// A replica's read pairs of the current hour.
#[derive(Debug)]
pub struct ReadsRecorder {
    replica: String,
    current: Mutex<(u64, BTreeSet<(String, String)>)>,
}

impl ReadsRecorder {
    pub fn new(replica: &str) -> ReadsRecorder {
        ReadsRecorder { replica: replica.to_string(), current: Mutex::new((0, BTreeSet::new())) }
    }

    pub fn replica(&self) -> &str {
        &self.replica
    }

    /// Records the refs a tool call's arguments name (`ref`: a string or a
    /// list; `repo`: a string, else `*`) at `now`. Returns the hour that
    /// ended and its pairs when this call started a new hour: write them.
    pub fn record(&self, args: &serde_json::Value, now: u64) -> Option<(u64, BTreeSet<(String, String)>)> {
        let refs: Vec<&str> = match args.get("ref") {
            Some(serde_json::Value::String(s)) => vec![s.as_str()],
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
            _ => Vec::new(),
        };
        let repo = match args.get("repo") {
            Some(serde_json::Value::String(s)) if s.trim() != ANY_REPO => s.trim(),
            _ => ANY_REPO,
        };
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        let ended = rotate(&mut cur, now);
        for r in refs.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
            cur.1.insert((repo.to_string(), r.to_string()));
        }
        ended
    }

    /// The pairs of an hour that ended before `now` (or, with `all`, of the
    /// current hour too: shutdown), taken out of the recorder.
    pub fn take(&self, now: u64, all: bool) -> Option<(u64, BTreeSet<(String, String)>)> {
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if all && !cur.1.is_empty() {
            return Some((cur.0, std::mem::take(&mut cur.1)));
        }
        rotate(&mut cur, now)
    }
}

/// Starts the hour of `now`; returns the previous hour's pairs when the
/// hour changed and it had any.
fn rotate(cur: &mut (u64, BTreeSet<(String, String)>), now: u64) -> Option<(u64, BTreeSet<(String, String)>)> {
    let hour = (now / HOUR) * HOUR;
    if cur.0 == hour {
        return None;
    }
    let prev = (cur.0, std::mem::take(&mut cur.1));
    cur.0 = hour;
    (!prev.1.is_empty()).then_some(prev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rueca_storage::MemoryStorage;
    use serde_json::json;

    #[test]
    fn paths_are_utc_days_and_hours() {
        // 2026-11-17T13:00:00Z
        assert_eq!(reads_path(1_794_920_400, "r1"), "reads/2026-11-17/13-r1.json");
        assert_eq!(reads_path(951_782_400, "r"), "reads/2000-02-29/00-r.json");
        assert_eq!(reads_path(0, "r"), "reads/1970-01-01/00-r.json");
    }

    #[test]
    fn recorder_rotates_hourly_and_the_window_reads_back() {
        let storage = MemoryStorage::new();
        let rec = ReadsRecorder::new("r1");
        let t0 = 1_794_920_400;
        assert!(rec.record(&json!({"repo": "github.com/a/b", "ref": "pr/1"}), t0 + 10).is_none());
        assert!(rec.record(&json!({"ref": ["pr/2", "main"]}), t0 + 20).is_none());
        assert!(rec.record(&json!({"query": "x"}), t0 + 30).is_none());
        let (hour, pairs) = rec.record(&json!({}), t0 + HOUR + 1).unwrap();
        assert_eq!((hour, pairs.len()), (t0, 3));
        write_reads(&storage, rec.replica(), hour, &pairs).unwrap();
        write_reads(&storage, rec.replica(), hour, &pairs).unwrap();
        let reads = last_reads(&storage, t0 - DAY, t0 + 2 * HOUR).unwrap();
        assert_eq!(last_read(&reads, "github.com/a/b", "pr/1"), Some(t0 + HOUR));
        assert_eq!(last_read(&reads, "github.com/x/y", "pr/2"), Some(t0 + HOUR), "`*` counts for every repository");
        assert_eq!(last_read(&reads, "github.com/x/y", "pr/1"), None);
        assert!(last_reads(&storage, t0 + HOUR, t0 + 2 * HOUR).unwrap().is_empty(), "outside the window");
        assert_eq!(prune_reads(&storage, t0).unwrap(), 0, "the same day is kept");
        assert_eq!(prune_reads(&storage, t0 + DAY).unwrap(), 1);
        assert!(rec.take(t0 + HOUR + 2, false).is_none());
        rec.record(&json!({"ref": "pr/3"}), t0 + HOUR + 5);
        assert_eq!(rec.take(t0 + HOUR + 6, true).unwrap().1.len(), 1);
    }
}
