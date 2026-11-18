# Recording script: Rueca on the tokio-rs organization

A 40 to 60 second screen recording of the [tokio-rs demo](tokio-rs.md):
one terminal command, three questions to an agent, a closing line. The
answers quoted below are the captured outputs of
`scripts/demo-tokio-rs.sh`; the agent's own wording will differ.

## Setup (once, off camera)

1. Build the binary as in [Install and index](tokio-rs.md#install-and-index)
   and run the demo, which clones, indexes and checks everything:

   ```sh
   export RUECA_DEMO_CACHE=${TMPDIR:-/tmp}/rueca-demo-cache
   RUECA=$(command -v rueca) scripts/demo-tokio-rs.sh
   ```

2. Register the organization with Claude Code. `RUECA_HOME` points the
   server at the demo's store; without it, the server builds a second copy
   of the index in `~/.cache/rueca` on first start.

   ```sh
   claude mcp add rueca -e RUECA_HOME=$RUECA_DEMO_CACHE/store \
     -- rueca mcp --org $RUECA_DEMO_CACHE/rueca-org.toml
   claude mcp list        # rueca should show as connected
   ```

3. Ask the three questions once, off camera, in a scratch session. This
   warms the file cache, so no take waits on a cold disk, and shows you
   what the agent will say.
4. Terminal: a monospace font at 16 to 18 pt, a dark or light theme with
   good contrast, about 120 columns. Clear the scrollback. Set the prompt
   to something short, such as `$ `.
5. Screen: terminal on the left half, Claude Code on the right half, both
   at the same font size. Start Claude Code in an empty directory so it
   does not reach for local files instead of Rueca.

## Timeline

### 0 to 8 s: the organization

In the terminal:

```sh
RUECA_HOME=$RUECA_DEMO_CACHE/store rueca status
```

Say: "These are eight Rust repositories from three GitHub organizations,
tokio, hyper, tower and the rest, indexed as one organization. tokio has
two refs: the current release and 1.53.3." Point at the line
`ref tokio-1.53.3 @a85c3f5baa29` and at `8 repositories + 1 refs, 75583
symbol documents`.

### 8 to 22 s: question 1

Type into Claude Code:

> Who drives mio's event loop in this org? Walk up from mio's Poll::poll.

Expect a `trace` call on `mio::poll::Poll.poll`, callers, depth 3 or 4
(it may search first). The key lines:

```
[0] github.com/tokio-rs/mio@101e5b7c92b1 (HEAD)
[1] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
1 [1] in d1 Method Driver.turn  tokio/src/runtime/io/driver.rs L185-258  →0 CALLS 0.85 type_resolved @199
4 [1] in d3 Method Driver.park  tokio/src/runtime/signal/mod.rs L91-94  →2 CALLS 0.85 type_resolved @92
6 [1] in d4 Method Driver.park  tokio/src/runtime/process.rs L31-34  →4 CALLS 0.85 type_resolved @32
```

Point at the `[0]` / `[1]` markers: the walk leaves mio and enters tokio.
Point at `type_resolved 0.85 @199`: every edge says how it was resolved
and on which line the call is. Say: "The I/O driver's turn, then the
signal driver, then the process driver."

### 22 to 36 s: question 2

> How is graceful shutdown wired in axum and tonic? Which code creates
> tokio watch channels?

Expect `search_symbols` for "graceful shutdown", then `trace` on
`tokio::sync::watch::channel`, callers. The key lines:

```
1 [1] in d1 Method Serve.run  axum/src/serve/mod.rs L179-193  →0 CALLS 0.9 import_resolved @186
2 [1] in d1 Method WithGracefulShutdown.run  axum/src/serve/mod.rs L267-304  →0 CALLS 0.9 import_resolved @275
3 [0] in d1 Method GracefulCoordinator.new  grpc/src/server/mod.rs L166-169  →0 CALLS 0.9 import_resolved @167
9 [0] in d1 Method Server.serve_internal  tonic/src/transport/server/mod.rs L774-918  →0 CALLS 0.9 import_resolved @858
```

Point at `[1]` (axum) and `[0]` (tonic) in one answer. Say: "One trace
from tokio's primitive finds the serve loops of both frameworks."

Alternative question 2, same slot:

> Who spawns tokio tasks across these repositories? Trace the callers of
> tokio's spawn.

Expect `search_symbols` or `get_symbol` for `spawn`, then `trace` on
`tokio::task::spawn::spawn`, callers. If the agent tries
`tokio::spawn` first, it gets "no symbol found" and searches; cut that
part. The key lines:

```
1 [2] in d1 Method WebSocketUpgrade.on_upgrade  axum/src/extract/ws.rs L347-411  →0 CALLS 0.9 import_resolved @359
2 [2] in d1 Method WithGracefulShutdown.run  axum/src/serve/mod.rs L267-304  →0 CALLS 0.9 import_resolved @276
6 [1] in d1 Function main  grpc-benchmark/src/bin/bench.rs L135-336  →0 CALLS 0.9 import_resolved @176
8 [0] in d1 Function fetch_url  examples/client.rs L42-81  →0 CALLS 0.9 import_resolved @50
(422 tests excluded; `include_tests: true` lists them)
```

Point at the three snapshot markers (axum `[2]`, tonic `[1]`, hyper
`[0]`). Say: "`spawn` sits inside a feature macro in tokio; Rueca opens
the macro body, so its 92 callers across the organization show up."

### 36 to 50 s: question 3

> What changed between tokio 1.53.3 and the current release around
> blocking acquire and lock?

Expect `search_symbols` with `ref: ["HEAD", "tokio-1.53.3"]`. If the
agent tries `ref: "tokio-1.54.1"`, the error lists the indexed refs
(`HEAD@b53b2001ed2c, tokio-1.53.3@a85c3f5baa29`) and it retries; cut
that part. The key lines:

```
  Method Semaphore.blocking_acquire L802-804  pub fn blocking_acquire(&self) -> Result<SemaphorePermit<'_>, AcquireError> changed
  Method Mutex.blocking_lock L520-522  pub fn blocking_lock(&self) -> MutexGuard<'_, T> [0,1]
  Method RwLock.blocking_write L877-879  pub fn blocking_write(&self) -> RwLockWriteGuard<'_, T> changed
```

Point at ` [0,1]` (identical in both releases) and ` changed`. Say it
straight: "`changed` means the file changed; the comparison is per file
today. `[0,1]` means the code did not move."

### 50 to 60 s: close

Say: "Rueca is a code graph for agents: symbols, calls and contracts
across repositories and refs, served over MCP. It is lexical today;
semantic search is next. On this organization it still misses
re-export paths such as `tokio::spawn` as names, and methods of
extension traits such as tower's `oneshot`. The numbers and the failures
are in the demo page."

## Avoid on camera

- Re-export paths as names: `tokio::spawn`, `tokio::net::TcpListener`
  and `tokio::time::sleep` answer "no symbol found"; only the module
  paths work (`tokio::task::spawn::spawn`,
  `tokio::net::tcp::listener::TcpListener.accept`,
  `tokio::time::sleep::sleep`).
- tower's `ServiceExt.oneshot` and other methods of extension traits
  implemented for every `Service`: 0 callers.
- "How does hyper read the request body": the lexical ranking returns FFI
  types and roadmap sections.
- Dangling contracts: all 19 are false positives (`.unwrap()` read as an
  RPC).
- Callers of hyper's `Connection.graceful_shutdown`: 1 for HTTP/1, 0 for
  HTTP/2; the `Pin::new(&mut conn)` call sites are not linked.

## Re-recording

The outputs are deterministic given the pins and the binary. Before a new
take, re-run `scripts/demo-tokio-rs.sh` (it re-checks every pin) and
compare the key lines above with `out/08-*.txt`, `out/10-*.txt`,
`out/13-*.txt` and `out/15-*.txt`. If the binary changed, update this
page from those files first. Record each question as its own take and
cut the agent's thinking time; do not speed up the answers.
