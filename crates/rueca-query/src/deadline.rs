//! The cooperative deadline of a call (RFC 0008 §6.5).
//!
//! A tool runs on a blocking thread that cannot be cancelled from outside,
//! so the engine stops itself: [`Deadline::check`] is one `Instant::now()`
//! comparison, made where work loops — per segment and per over-fetch round
//! of a symbol search, per file of a content scan, per view a call opens —
//! and, through [`Deadline::tick`], every [`CHECK_EVERY`] items of the
//! cheaper loops (`trace`'s traversal, outlines, multi-ref collapse).

use std::fmt;
use std::time::{Duration, Instant};

use rueca_segment::SegmentError;

/// How many items a cheap loop handles between two checks.
pub(crate) const CHECK_EVERY: usize = 64;

/// The error of a call whose deadline passed: names the deadline and how to
/// ask for less. Callers recognize it with `anyhow::Error::downcast_ref`
/// (an HTTP transport answers it with its own error code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineExceeded {
    /// The time the call was given: from when it started to its deadline.
    pub budget: Duration,
}

impl fmt::Display for DeadlineExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "deadline of {} exceeded; narrow `repo` or lower `limit`", format_duration(self.budget))
    }
}

impl std::error::Error for DeadlineExceeded {}

/// `1ms`, `250ms`, `10s`, `2.5s`: milliseconds under a second, else
/// seconds to a tenth.
pub(crate) fn format_duration(d: Duration) -> String {
    if !d.is_zero() && d < Duration::from_millis(1) {
        return format!("{}us", d.as_micros().max(1));
    }
    let ms = d.as_millis() + u128::from(d.subsec_nanos() % 1_000_000 >= 500_000);
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let tenths = (ms + 50) / 100;
    if tenths.is_multiple_of(10) { format!("{}s", tenths / 10) } else { format!("{}.{}s", tenths / 10, tenths % 10) }
}

/// When a call must stop, if ever.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Deadline {
    at: Option<Instant>,
    budget: Duration,
}

impl Deadline {
    /// A deadline at `at`, the call starting now.
    pub(crate) fn new(at: Option<Instant>) -> Deadline {
        let budget = at.map(|a| a.saturating_duration_since(Instant::now())).unwrap_or_default();
        Deadline { at, budget }
    }

    /// The instant, for the segment queries.
    pub(crate) fn at(&self) -> Option<Instant> {
        self.at
    }

    /// `Err(DeadlineExceeded)` once the deadline has passed.
    pub(crate) fn check(&self) -> anyhow::Result<()> {
        match self.at {
            Some(at) if Instant::now() >= at => Err(self.exceeded().into()),
            _ => Ok(()),
        }
    }

    /// [`Deadline::check`] on every [`CHECK_EVERY`]th item (`i` = 0, 64, …).
    pub(crate) fn tick(&self, i: usize) -> anyhow::Result<()> {
        if i.is_multiple_of(CHECK_EVERY) { self.check() } else { Ok(()) }
    }

    fn exceeded(&self) -> DeadlineExceeded {
        DeadlineExceeded { budget: self.budget }
    }

    /// `err`, with a segment search stopped by the deadline
    /// ([`SegmentError::DeadlineExceeded`]) turned into [`DeadlineExceeded`].
    pub(crate) fn explain(&self, err: anyhow::Error) -> anyhow::Error {
        match err.downcast_ref::<SegmentError>() {
            Some(SegmentError::DeadlineExceeded) => self.exceeded().into(),
            _ => err,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let cases = [
            (Duration::ZERO, "0ms"),
            (Duration::from_nanos(1), "1us"),
            (Duration::from_micros(999), "999us"),
            (Duration::from_millis(250), "250ms"),
            (Duration::from_millis(9_998), "10s"),
            (Duration::from_secs(10), "10s"),
            (Duration::from_millis(2_460), "2.5s"),
        ];
        for (d, want) in cases {
            assert_eq!(format_duration(d), want, "{d:?}");
        }
    }

    #[test]
    fn checks() {
        assert!(Deadline::default().check().is_ok(), "no deadline");
        assert!(Deadline::new(Some(Instant::now() + Duration::from_secs(60))).check().is_ok());
        let past = Deadline::new(Some(Instant::now()));
        let err = past.check().unwrap_err();
        let exceeded = err.downcast_ref::<DeadlineExceeded>().copied().unwrap();
        assert_eq!(exceeded.budget, Duration::ZERO);
        assert!(err.to_string().starts_with("deadline of 0ms exceeded; narrow `repo` or lower `limit`"), "{err}");
        assert!(past.tick(0).is_err() && past.tick(1).is_ok() && past.tick(CHECK_EVERY).is_err());
        let explained = past.explain(SegmentError::DeadlineExceeded.into());
        assert!(explained.downcast_ref::<DeadlineExceeded>().is_some());
        let other = past.explain(anyhow::anyhow!("other"));
        assert_eq!(other.to_string(), "other");
    }
}
