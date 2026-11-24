//! Engine-neutral bookkeeping of one run: host calls against the call
//! limit, captured logs against the log cap, and the deadline.

use std::time::Instant;

use serde_json::Value;

use crate::{ErrorKind, Limits, Outcome, ScriptError};

/// The counters and captured output of one run.
#[derive(Debug)]
pub(crate) struct RunState {
    deadline: Instant,
    max_calls: u32,
    max_log_bytes: usize,
    pub(crate) calls: u32,
    logs: Vec<String>,
    log_bytes: usize,
    logs_truncated: bool,
    /// Set once the call limit has been hit: the run fails even if the
    /// script catches the error.
    pub(crate) call_limit_hit: bool,
}

impl RunState {
    pub(crate) fn new(limits: &Limits, started: Instant) -> RunState {
        RunState {
            deadline: started + limits.wall_time,
            max_calls: limits.max_calls,
            max_log_bytes: limits.max_log_bytes,
            calls: 0,
            logs: Vec::new(),
            log_bytes: 0,
            logs_truncated: false,
            call_limit_hit: false,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Counts a host call, or refuses it over the call limit.
    pub(crate) fn admit(&mut self) -> Result<(), String> {
        if self.calls >= self.max_calls {
            self.call_limit_hit = true;
            return Err(format!("call limit of {} host calls exceeded", self.max_calls));
        }
        self.calls += 1;
        Ok(())
    }

    /// [`RunState::admit`], then the host call.
    #[cfg(feature = "starlark")]
    pub(crate) fn call(&mut self, host: &mut dyn crate::Host, tool: &str, args: Value) -> Result<Value, String> {
        self.admit()?;
        host.call(tool, args)
    }

    /// Captures one log line within the log cap (cut at a character
    /// boundary when it only partly fits); nothing after the first line
    /// that did not fit.
    pub(crate) fn log(&mut self, line: &str) {
        if self.logs_truncated {
            return;
        }
        let room = self.max_log_bytes.saturating_sub(self.log_bytes);
        if line.len() <= room {
            self.log_bytes += line.len();
            self.logs.push(line.to_string());
            return;
        }
        self.logs_truncated = true;
        let mut cut = room;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut > 0 {
            self.log_bytes += cut;
            self.logs.push(line[..cut].to_string());
        }
    }

    pub(crate) fn finish(self, result: Value) -> Outcome {
        Outcome { result, logs: self.logs, logs_truncated: self.logs_truncated, calls: self.calls }
    }

    /// The call-limit error, when the limit was hit during the run.
    pub(crate) fn call_limit_error(&self) -> Option<ScriptError> {
        self.call_limit_hit.then(|| {
            ScriptError::new(ErrorKind::CallLimit, format!("call limit of {} host calls exceeded", self.max_calls))
        })
    }
}

/// Refuses a script over [`Limits::max_script_bytes`].
pub(crate) fn check_size(script: &str, limits: &Limits) -> Result<(), ScriptError> {
    if script.len() > limits.max_script_bytes {
        return Err(ScriptError::new(
            ErrorKind::ScriptTooLarge,
            format!("script is {} bytes; the limit is {}", script.len(), limits.max_script_bytes),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_cap_cuts_at_char_boundary() {
        let limits = Limits { max_log_bytes: 5, ..Limits::default() };
        let mut s = RunState::new(&limits, Instant::now());
        s.log("abc");
        s.log("dé£");
        s.log("x");
        let o = s.finish(Value::Null);
        assert_eq!(o.logs, vec!["abc".to_string(), "d".to_string()]);
        assert!(o.logs_truncated);
    }
}
