//! The stdio transport (`docs/mcp-protocol.md` §2 and §8): newline-delimited
//! JSON-RPC over any byte stream pair, std threads only.
//!
//! - One reader (the calling thread) reads line by line and feeds
//!   [`RuecaServer::handle`] in order.
//! - Each `tools/call` runs on its own thread, at most [`MAX_IN_FLIGHT`] at a
//!   time; at the cap the reader waits for a slot.
//! - One writer at a time: a line is never interleaved with another, and is
//!   flushed as soon as it is written.
//! - In-flight ids live in a set: `notifications/cancelled` removes the id
//!   and the call's response is dropped when it comes.
//! - At end of input, in-flight calls get up to [`DRAIN`] to answer; then
//!   [`serve`] returns and later responses are dropped.

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::RuecaServer;
use crate::jsonrpc::{Outgoing, RequestId, parse_line};
use crate::protocol::{Session, Step};
use crate::tools::ToolCall;

/// Longest accepted line (16 MiB, as rmcp). A longer one is discarded up to
/// its newline, logged, and gets no reply.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// Most tool calls running at once (tokio's default `max_blocking_threads`,
/// what rmcp ran them on).
pub const MAX_IN_FLIGHT: usize = 512;

/// How long in-flight calls may still answer after the input ends.
pub const DRAIN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_line: usize,
    pub max_in_flight: usize,
    pub drain: Duration,
}

impl Limits {
    pub const DEFAULT: Limits = Limits { max_line: MAX_LINE, max_in_flight: MAX_IN_FLIGHT, drain: DRAIN };
}

/// Serves MCP over a byte stream pair until `input` ends (EOF, or a read
/// error), then waits up to [`DRAIN`] for in-flight tool calls. Used by
/// [`crate::serve_stdio`] on stdin/stdout and by tests over
/// [`std::io::pipe`]. Blocks the calling thread. `output` is closed (dropped)
/// before it returns.
pub fn serve(server: RuecaServer, input: impl Read, output: impl Write + Send + 'static) -> anyhow::Result<()> {
    serve_with(server, input, output, Limits::DEFAULT)
}

pub(crate) fn serve_with(
    server: RuecaServer,
    input: impl Read,
    output: impl Write + Send + 'static,
    limits: Limits,
) -> anyhow::Result<()> {
    let shared = Arc::new(Shared::new(Box::new(output)));
    let mut input = BufReader::with_capacity(64 * 1024, input);
    let mut session = Session::new();
    let mut line = Vec::new();
    while shared.is_open() {
        match read_line(&mut input, &mut line, limits.max_line) {
            Ok(LineRead::Line) => {
                let step = server.handle(parse_line(&line), &mut session);
                shared.apply(step, &server, limits.max_in_flight);
            }
            Ok(LineRead::Oversized) => {
                tracing::error!(
                    max_line = limits.max_line,
                    "incoming message exceeds the maximum line length; discarded"
                );
            }
            Ok(LineRead::Eof) => break,
            Err(err) => {
                tracing::error!(error = %err, "cannot read the input; stopping");
                break;
            }
        }
    }
    shared.drain(limits.drain);
    Ok(())
}

enum LineRead {
    Line,
    Oversized,
    Eof,
}

/// Reads one `\n`-terminated line into `buf` (without the `\n`). A line
/// longer than `max` is consumed but not kept. An unterminated last line is
/// dropped (EOF).
fn read_line(input: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> io::Result<LineRead> {
    buf.clear();
    let mut oversized = false;
    loop {
        let chunk = match input.fill_buf() {
            Ok(chunk) => chunk,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if chunk.is_empty() {
            return Ok(LineRead::Eof);
        }
        let (consumed, ended) = take_chunk(buf, chunk, max, &mut oversized);
        input.consume(consumed);
        if ended {
            return Ok(LineRead::ended(oversized));
        }
    }
}

/// Appends `chunk` up to its first `\n` to `buf`, unless the line has grown
/// past `max` (then `buf` is emptied and the rest of the line skipped).
/// Returns the bytes consumed and whether the line ended.
fn take_chunk(buf: &mut Vec<u8>, chunk: &[u8], max: usize, oversized: &mut bool) -> (usize, bool) {
    let newline = chunk.iter().position(|&b| b == b'\n');
    let body = &chunk[..newline.unwrap_or(chunk.len())];
    *oversized = *oversized || buf.len() + body.len() > max;
    if *oversized {
        buf.clear();
    } else {
        buf.extend_from_slice(body);
    }
    (newline.map_or(chunk.len(), |i| i + 1), newline.is_some())
}

impl LineRead {
    fn ended(oversized: bool) -> LineRead {
        if oversized { LineRead::Oversized } else { LineRead::Line }
    }
}

/// State shared by the reader and the tool-call threads.
struct Shared {
    /// `None` once the output failed or [`serve`] returned.
    out: Mutex<Option<Box<dyn Write + Send>>>,
    flight: Mutex<Flight>,
    changed: Condvar,
}

#[derive(Default)]
struct Flight {
    /// Requests whose response is still wanted.
    ids: HashSet<RequestId>,
    /// Tool-call threads still running (cancelled ones included).
    running: usize,
    /// Responses being written right now.
    writing: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn new(out: Box<dyn Write + Send>) -> Shared {
        Shared { out: Mutex::new(Some(out)), flight: Mutex::new(Flight::default()), changed: Condvar::new() }
    }

    fn is_open(&self) -> bool {
        lock(&self.out).is_some()
    }

    fn apply(self: &Arc<Self>, step: Step, server: &RuecaServer, max_in_flight: usize) {
        match step {
            Step::Reply(msg) => self.write(&msg),
            Step::Call(call) => self.start(call, server, max_in_flight),
            Step::Cancel(id) => self.cancel(&id),
            Step::Silent => {}
        }
    }

    /// Writes one line and flushes it. A failed write closes the output: the
    /// client is gone, so the reader stops.
    fn write(&self, msg: &Outgoing) {
        let mut out = lock(&self.out);
        let Some(w) = out.as_mut() else { return };
        if let Err(err) = w.write_all(&msg.to_line()).and_then(|()| w.flush()) {
            tracing::error!(error = %err, "cannot write to the output; stopping");
            *out = None;
        }
    }

    /// Registers the call as in flight and runs it on its own thread,
    /// waiting first for a free slot.
    fn start(self: &Arc<Self>, call: ToolCall, server: &RuecaServer, max_in_flight: usize) {
        {
            let flight = lock(&self.flight);
            let mut flight =
                self.changed.wait_while(flight, |f| f.running >= max_in_flight).unwrap_or_else(PoisonError::into_inner);
            flight.running += 1;
            flight.ids.insert(call.id.clone());
        }
        let (shared, worker_server, worker_call) = (Arc::clone(self), server.clone(), call.clone());
        let spawned = std::thread::Builder::new()
            .name("rueca-tool-call".into())
            .spawn(move || shared.finish(worker_call.run(&worker_server)));
        if let Err(err) = spawned {
            tracing::error!(error = %err, "cannot start a tool-call thread; running the call on the reader");
            self.finish(call.run(server));
        }
    }

    /// A call is done: write its response unless it was cancelled (or a
    /// duplicate id already answered).
    fn finish(&self, msg: Outgoing) {
        let wanted = {
            let mut flight = lock(&self.flight);
            flight.running -= 1;
            let wanted = msg.id().is_some_and(|id| flight.ids.remove(id));
            flight.writing += usize::from(wanted);
            wanted
        };
        self.changed.notify_all();
        if !wanted {
            tracing::debug!(id = ?msg.id(), "dropping response for cancelled request");
            return;
        }
        self.write(&msg);
        lock(&self.flight).writing -= 1;
        self.changed.notify_all();
    }

    fn cancel(&self, id: &RequestId) {
        if lock(&self.flight).ids.remove(id) {
            tracing::debug!(%id, "request cancelled");
        }
        self.changed.notify_all();
    }

    /// Waits up to `timeout` for every wanted response to be written, then
    /// closes the output.
    fn drain(&self, timeout: Duration) {
        let flight = lock(&self.flight);
        let (flight, waited) = self
            .changed
            .wait_timeout_while(flight, timeout, |f| !f.ids.is_empty() || f.writing > 0)
            .unwrap_or_else(PoisonError::into_inner);
        if waited.timed_out() {
            tracing::warn!(pending = flight.ids.len(), "input closed; dropping responses of calls still running");
        }
        drop(flight);
        *lock(&self.out) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IndexGate;
    use serde_json::{Value, json};
    use std::io::Cursor;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
    use std::thread::JoinHandle;
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(10);

    /// A server on an empty store whose tool calls block until the returned
    /// gate finishes.
    fn gated_server() -> (RuecaServer, Arc<IndexGate>, tempfile::TempDir) {
        let gate = Arc::new(IndexGate::named("the test gate".into()));
        let store = tempfile::tempdir().unwrap();
        let engine = rueca_query::Engine::new(store.path().to_path_buf(), None).unwrap();
        (RuecaServer::with_index_gate(engine, Some(gate.clone())), gate, store)
    }

    struct Harness {
        input: Option<io::PipeWriter>,
        lines: Receiver<String>,
        done: JoinHandle<anyhow::Result<()>>,
    }

    impl Harness {
        fn start(server: RuecaServer, limits: Limits) -> Harness {
            let (in_r, in_w) = io::pipe().unwrap();
            let (out_r, out_w) = io::pipe().unwrap();
            let done = std::thread::spawn(move || serve_with(server, in_r, out_w, limits));
            let (tx, lines) = channel();
            std::thread::spawn(move || {
                for line in BufReader::new(out_r).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
            Harness { input: Some(in_w), lines, done }
        }

        fn send(&mut self, line: &str) {
            let w = self.input.as_mut().unwrap();
            w.write_all(line.as_bytes()).unwrap();
            w.write_all(b"\n").unwrap();
        }

        fn request(&mut self, id: i64, method: &str, params: Value) {
            self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string());
        }

        fn recv(&self) -> Value {
            serde_json::from_str(&self.lines.recv_timeout(WAIT).expect("a response")).unwrap()
        }

        fn quiet(&self, d: Duration) -> bool {
            matches!(self.lines.recv_timeout(d), Err(RecvTimeoutError::Timeout))
        }

        /// Closes the input; returns what was written until the output closed.
        fn close(mut self) -> Vec<String> {
            drop(self.input.take());
            self.done.join().unwrap().unwrap();
            self.lines.iter().collect()
        }
    }

    fn legacy(h: &mut Harness) {
        h.request(
            1,
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
        );
        assert_eq!(h.recv()["id"], 1);
    }

    fn call(h: &mut Harness, id: i64) {
        h.request(id, "tools/call", json!({"name": "search_symbols", "arguments": {"query": "x"}}));
    }

    #[test]
    fn cancellation_drops_the_response() {
        let (server, gate, _store) = gated_server();
        let mut h = Harness::start(server, Limits::DEFAULT);
        legacy(&mut h);
        call(&mut h, 2);
        h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#);
        h.request(3, "ping", json!({}));
        assert_eq!(h.recv()["id"], 3, "the call is still blocked");
        gate.finish(Ok(()));
        call(&mut h, 4);
        assert_eq!(h.recv()["id"], 4, "the cancelled call never answers");
        assert!(h.close().is_empty());
    }

    #[test]
    fn eof_drains_in_flight_calls() {
        let (server, gate, _store) = gated_server();
        let mut h = Harness::start(server, Limits::DEFAULT);
        legacy(&mut h);
        call(&mut h, 2);
        call(&mut h, 3);
        h.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}"#);
        let g = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            g.finish(Ok(()));
        });
        let rest = h.close();
        assert_eq!(rest.len(), 1, "{rest:?}");
        let v: Value = serde_json::from_str(&rest[0]).unwrap();
        assert_eq!(v["id"], 2);
        assert_eq!(v["result"]["isError"], true, "empty store: {v}");
    }

    #[test]
    fn eof_drain_gives_up_after_the_timeout() {
        let (server, _gate, _store) = gated_server();
        let mut h = Harness::start(server, Limits { drain: Duration::from_millis(100), ..Limits::DEFAULT });
        legacy(&mut h);
        call(&mut h, 2);
        let started = Instant::now();
        assert!(h.close().is_empty());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_reader_waits_for_a_free_slot() {
        let (server, gate, _store) = gated_server();
        let mut h = Harness::start(server, Limits { max_in_flight: 1, ..Limits::DEFAULT });
        legacy(&mut h);
        call(&mut h, 2);
        call(&mut h, 3);
        h.request(4, "ping", json!({}));
        assert!(h.quiet(Duration::from_millis(200)), "the reader is blocked at the cap");
        gate.finish(Ok(()));
        let mut ids: Vec<i64> = (0..3).map(|_| h.recv()["id"].as_i64().unwrap()).collect();
        ids.sort_unstable();
        assert_eq!(ids, [2, 3, 4]);
        assert!(h.close().is_empty());
    }

    #[test]
    fn framing_limits() {
        let (server, _gate, _store) = gated_server();
        let mut h = Harness::start(server, Limits { max_line: 64, ..Limits::DEFAULT });
        h.send(&format!(r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{}"}}}}"#, "x".repeat(100)));
        h.request(2, "ping", json!({}));
        assert_eq!(h.recv()["id"], 2, "the oversized line got no reply");
        h.input.as_mut().unwrap().write_all(br#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#).unwrap();
        assert!(h.close().is_empty(), "an unterminated last line is dropped");
    }

    #[test]
    fn read_line_splits_lines() {
        let mut input = BufReader::with_capacity(4, Cursor::new(b"ab\n\ncdefgh\nijk".to_vec()));
        let mut buf = Vec::new();
        let mut got = Vec::new();
        loop {
            match read_line(&mut input, &mut buf, 5).unwrap() {
                LineRead::Line => got.push(String::from_utf8(buf.clone()).unwrap()),
                LineRead::Oversized => got.push("<oversized>".into()),
                LineRead::Eof => break,
            }
        }
        assert_eq!(got, ["ab", "", "<oversized>"]);
    }

    /// A writer that always fails, like a closed stdout.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_broken_output_stops_the_reader() {
        let (server, _gate, _store) = gated_server();
        let (input, mut client) = io::pipe().unwrap();
        client.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n").unwrap();
        // The input stays open: only the failed write can end the loop.
        serve_with(server, input, Broken, Limits::DEFAULT).unwrap();
        drop(client);
    }
}
