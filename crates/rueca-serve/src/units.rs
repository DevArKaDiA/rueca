//! Durations (`"12s"`, `"500ms"`, `"7d"`) and sizes (`"1MiB"`, `"40GiB"`)
//! as `rueca-service.toml` writes them: a small parser of our own
//! (`humantime` is unmaintained).

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

/// A duration written as an integer and a unit: `ms`, `s`, `m`, `h`, `d`
/// (`"45s"`, `"8h"`, `"7d"`). One unit per value; `"0"` alone is zero.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let t = text.trim();
    if t == "0" {
        return Ok(Duration::ZERO);
    }
    let split =
        t.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| format!("`{text}` has no unit (ms, s, m, h, d)"))?;
    let (num, unit) = t.split_at(split);
    let n: u64 = num.parse().map_err(|_| format!("`{text}` is not a duration (e.g. \"12s\", \"500ms\", \"7d\")"))?;
    let secs = |k: u64| n.checked_mul(k).map(Duration::from_secs);
    let d = match unit.trim() {
        "ms" => Some(Duration::from_millis(n)),
        "s" => secs(1),
        "m" => secs(60),
        "h" => secs(3600),
        "d" => secs(86_400),
        other => return Err(format!("`{text}`: unknown duration unit `{other}` (ms, s, m, h, d)")),
    };
    d.ok_or_else(|| format!("`{text}` is too large"))
}

/// A size in bytes: an integer with an optional unit, binary (`KiB`,
/// `MiB`, `GiB`, `TiB`) or decimal (`B`, `KB`, `MB`, `GB`, `TB`).
pub fn parse_size(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: u64 = num.parse().map_err(|_| format!("`{text}` is not a size (e.g. \"1MiB\", \"40GiB\")"))?;
    let k: u64 = match unit.trim() {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        other => return Err(format!("`{text}`: unknown size unit `{other}` (B, KiB, MiB, GiB, TiB, KB, MB, GB, TB)")),
    };
    n.checked_mul(k).ok_or_else(|| format!("`{text}` is too large"))
}

/// A configuration duration (a string, see [`parse_duration`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Dur(pub Duration);

impl fmt::Debug for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl Dur {
    pub const fn secs(s: u64) -> Dur {
        Dur(Duration::from_secs(s))
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Dur, D::Error> {
        let s = String::deserialize(d)?;
        parse_duration(&s).map(Dur).map_err(serde::de::Error::custom)
    }
}

/// A configuration size in bytes (a string such as `"1MiB"`, or an
/// integer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Size(pub u64);

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Size, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(Size(n)),
            Raw::Text(s) => parse_size(&s).map(Size).map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let cases = [
            ("12s", Some(Duration::from_secs(12))),
            ("500ms", Some(Duration::from_millis(500))),
            ("30m", Some(Duration::from_secs(1800))),
            ("4h", Some(Duration::from_secs(14_400))),
            ("7d", Some(Duration::from_secs(604_800))),
            ("0", Some(Duration::ZERO)),
            (" 5s ", Some(Duration::from_secs(5))),
            ("12", None),
            ("s", None),
            ("1.5s", None),
            ("3w", None),
            ("-1s", None),
            ("99999999999999999999d", None),
        ];
        for (text, want) in cases {
            assert_eq!(parse_duration(text).ok(), want, "{text}");
        }
    }

    #[test]
    fn sizes() {
        let cases = [
            ("1MiB", Some(1 << 20)),
            ("40GiB", Some(40 << 30)),
            ("2KB", Some(2_000)),
            ("123", Some(123)),
            ("7B", Some(7)),
            ("1mib", None),
            ("MiB", None),
            ("1.5MiB", None),
            ("99999999TiB", None),
        ];
        for (text, want) in cases {
            assert_eq!(parse_size(text).ok(), want, "{text}");
        }
    }
}
