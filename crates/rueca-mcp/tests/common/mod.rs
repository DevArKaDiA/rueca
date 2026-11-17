//! An MCP client over in-process pipes: the server runs [`rueca_mcp::serve`]
//! on its own thread, the test writes lines to its input and reads the
//! lines it writes.

use std::io::{BufRead, BufReader, PipeWriter, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long a test waits for one line (tool calls may wait for an index).
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Client {
    input: Option<PipeWriter>,
    lines: Receiver<String>,
    server: JoinHandle<anyhow::Result<()>>,
}

impl Client {
    pub fn start(server: rueca_mcp::RuecaServer) -> Client {
        let (server_in, input) = std::io::pipe().unwrap();
        let (output, server_out) = std::io::pipe().unwrap();
        let server = std::thread::spawn(move || rueca_mcp::serve(server, server_in, server_out));
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Client { input: Some(input), lines, server }
    }

    /// Writes `line` and its `\n`.
    pub fn send_line(&mut self, line: &str) {
        let input = self.input.as_mut().expect("input open");
        input.write_all(line.as_bytes()).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
    }

    /// The next line the server writes; `None` once its output is closed.
    pub fn next_line(&self) -> Option<String> {
        match self.lines.recv_timeout(TIMEOUT) {
            Ok(line) => Some(line),
            Err(RecvTimeoutError::Disconnected) => None,
            Err(RecvTimeoutError::Timeout) => panic!("no line from the server in {TIMEOUT:?}"),
        }
    }

    /// Closes the input (EOF); returns what `serve` returned and every line
    /// written until the output closed.
    pub fn close(mut self) -> (anyhow::Result<()>, Vec<String>) {
        drop(self.input.take());
        let outcome = self.server.join().expect("serve does not panic");
        (outcome, self.lines.iter().collect())
    }
}
