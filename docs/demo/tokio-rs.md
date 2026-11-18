# Demo: the tokio-rs organization

This is Rueca's organization graph over a real, public family of Rust
repositories: tokio and the crates around it, eight repositories from
three GitHub organizations, with a second ref of tokio indexed next to the
current release. Everything below is the output of
[`scripts/demo-tokio-rs.sh`](../../scripts/demo-tokio-rs.sh), run on
2026-11-18 on a MacBook Pro M1 Pro with 32 GB of memory, with
`rueca 0.1.0-alpha.2` built from commit `4905a37` of `main`. The query
outputs are byte-identical across runs; only the timings vary. An agent
reads the same answers over MCP with `rueca mcp --org <file>`.

## The organization

tokio is the async runtime; mio is the event loop under it; bytes holds
the byte buffers; tracing is the instrumentation layer. hyper (an HTTP
implementation) and tonic (gRPC) live in the hyperium organization, tower
(the `Service` and `Layer` abstractions) in tower-rs, and axum (an HTTP
router on hyper and tower) back in tokio-rs. They depend on each other
through Cargo, so the link layer has package edges between them: tonic
calls tower, tokio and bytes; tokio calls mio and bytes. tonic also ships
`.proto` files, and its examples and tests implement and call the services
they declare, so contracts (RPC providers and consumers) show up too.

I pinned the latest release tag of each main crate on 2026-11-17. The
script checks every tag against the commit below and stops if a tag has
moved.

| Repository id | Tag | Commit |
|---|---|---|
| `github.com/tokio-rs/tokio` | `tokio-1.54.1` | `b53b2001ed2ccfd9b87e32a0c6da1a1eebdc80b8` |
| `github.com/tokio-rs/tokio` (second ref) | `tokio-1.53.3` | `a85c3f5baa298a6b554fd3700daf0342289881af` |
| `github.com/tokio-rs/mio` | `v1.2.5` | `101e5b7c92b1b5f8abc0ba3eb81b264943e13e37` |
| `github.com/tokio-rs/bytes` | `v1.12.2` | `67edc2b999d669eb4f471aff576e213dbcb58871` |
| `github.com/tokio-rs/tracing` | `tracing-0.1.44` | `2d55f6faf9be83e7e4634129fb96813241aac2b8` |
| `github.com/hyperium/hyper` | `v1.12.0` | `7f67699ce32d717081deb7018b4da7d679277ac0` |
| `github.com/tower-rs/tower` | `tower-0.5.3` | `4b0a6b0e688bd177eb2c9c97f5268dd9703c66fc` |
| `github.com/tokio-rs/axum` | `axum-v0.8.9` | `c59208c86fded335cd85e388030ad59347b0e5ae` |
| `github.com/hyperium/tonic` | `tonic-v0.14.9` | `4c0f455c264fe2ec9f9ab5d0cc7fd2824d5daf70` |

The clones take 256 MB (tonic 166 MB, tokio 28 MB, hyper 24 MB, tracing
14 MB, axum 10 MB, tower 5.2 MB, mio 5.1 MB, bytes 3.8 MB). They are full
clones, so indexing the second tokio ref needs no network.

The script writes this organization file, with absolute paths where this
page shows `$RUECA_DEMO_CACHE`:

```toml
name = "tokio-rs"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/tokio"
id = "github.com/tokio-rs/tokio"
refs = ["tokio-1.53.3"]

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/mio"
id = "github.com/tokio-rs/mio"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/bytes"
id = "github.com/tokio-rs/bytes"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/tracing"
id = "github.com/tokio-rs/tracing"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/hyper"
id = "github.com/hyperium/hyper"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/tower"
id = "github.com/tower-rs/tower"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/axum"
id = "github.com/tokio-rs/axum"

[[repo]]
path = "$RUECA_DEMO_CACHE/repos/tonic"
id = "github.com/hyperium/tonic"
```

Each checkout is detached at its pinned commit, so the default ref of
every repository is named `HEAD`, not after its tag: `ref: "HEAD"` reads
tokio 1.54.1, and `ref: "tokio-1.54.1"` answers that the ref is not
indexed.

## Install and index

The demo needs the Rust language pack, refs over an organization,
contracts, and the Rust fixes of
[PR #68](https://github.com/DevArKaDiA/rueca/pull/68) (items inside macro
bodies, attribute macros, value and type namespaces, crate-scoped types).
All of them landed on `main` after the `0.1.0-alpha.2` release, so for now
build from source; the binary still reports `0.1.0-alpha.2`:

```sh
cargo install --git https://github.com/DevArKaDiA/rueca --rev 4905a37 --locked rueca
```

Once a later release is out, the usual install lines from the
[README](../../README.md#install) will do:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/DevArKaDiA/rueca/releases/download/v0.1.0-alpha.2/rueca-installer.sh | sh
cargo install rueca@0.1.0-alpha.2 --locked
```

Then run the whole demo (clone, index, queries, checks):

```sh
RUECA=$(command -v rueca) scripts/demo-tokio-rs.sh
```

The script keeps its own store in `$RUECA_HOME`, by default
`$RUECA_DEMO_CACHE/store`, and its working directory `$RUECA_DEMO_CACHE`
defaults to `${TMPDIR:-/tmp}/rueca-demo-cache`. The equivalent manual
commands, after the clones exist:

```sh
export RUECA_DEMO_CACHE=${TMPDIR:-/tmp}/rueca-demo-cache
export RUECA_HOME=$RUECA_DEMO_CACHE/store
rueca index --org "$RUECA_DEMO_CACHE/rueca-org.toml"
rueca status
rueca links --org tokio-rs
rueca contracts --org tokio-rs
```

### Index numbers

The first index of a fresh store, from `out/index.json`:

| Repository | Files | Nodes | Edges | Index ms |
|---|---:|---:|---:|---:|
| tokio | 886 | 16,443 | 56,615 | 993 |
| mio | 89 | 2,167 | 7,589 | 214 |
| bytes | 47 | 1,270 | 3,726 | 161 |
| tracing | 345 | 6,966 | 24,007 | 441 |
| hyper | 139 | 4,241 | 11,226 | 289 |
| tower | 162 | 2,035 | 5,477 | 186 |
| axum | 488 | 6,441 | 17,387 | 349 |
| tonic | 815 | 20,355 | 52,822 | 972 |
| **8 repositories** | **2,971** | **59,918** | **178,849** | |
| tokio at `tokio-1.53.3` | 866 | 15,665 | 53,210 | 979 |

Nodes are every node of the graph, files included; `list_repositories`
reports a smaller count of symbols. The `tokio-1.53.3` ref exported 867
files from the git object database and indexed 866 of them; 504 came from
the facts cache, because their blobs are the same in both releases. Its
979 ms include the 248 ms export.

Rueca reports 7,145 ms for the whole run: 3,613 ms for the eight
repositories, 982 ms for the ref and 2,549 ms for the organization layer
(332 ms profiles, 1,696 ms links). Wall time was 7,162 ms. Across my runs
on a fresh store, wall time was 7.2 to 8.0 s; 29.8 s the first time, with
a cold file cache right after cloning. `link_errors` and `ref_errors` were
empty.

Running `rueca index --org` again with nothing changed took 5,060 ms
(5.1 to 5.4 s across runs). It is not a no-op: every repository is
replaced and the generation goes up; only the ref is skipped
(`out/index-again.txt`, first lines):

```
indexed /private/tmp/rueca-demo-cache/repos/tokio (github.com/tokio-rs/tokio @ b53b2001ed2c): 886 files, 16443 nodes, 56615 edges in 581 ms
[…]
indexed ref tokio-1.53.3 of github.com/tokio-rs/tokio (tag @ a85c3f5baa29): unchanged in 11 ms
organization tokio-rs: generation 2, 8 repositories (0 added, 8 replaced, 0 removed, 0 unchanged) in 2385 ms
  refs:       1 (0 added, 0 replaced, 0 removed, 1 unchanged)
```

The store was 138 MB after the first index and 194 MB after the second;
it stays there on later runs. The organization symbol index holds 75,583
documents in 15.1 MB (`rueca status`).

### Links

The organization update reports 10,837 link edges: 9,550 between the
default refs and 1,287 for the `tokio-1.53.3` entry. All of them are
package edges (`via package`); there are no cross-repository contract
edges here, because every contract's providers and consumers sit in the
same repository (tonic). The largest consumer → provider pairs:

| Consumer → provider | Edges |
|---|---:|
| tonic → tokio | 2,687 |
| tonic → tower | 1,151 |
| hyper → tokio | 898 |
| tonic → bytes | 702 |
| tokio → bytes | 684 |
| axum → tokio | 516 |
| hyper → bytes | 490 |
| tokio → tracing | 487 |
| axum → tower | 467 |
| tower → tokio | 440 |
| axum → tracing | 263 |
| axum → bytes | 167 |
| tokio → mio | 158 |
| tonic → hyper | 149 |

The full list is in `out/links-summary.txt`. The provider is the edge's
other repository: a `DECORATES` edge runs from tokio-macros to the
consumer's function, so the 509 `#[tokio::main]` and `#[tokio::test]`
edges of tonic count under tonic → tokio. By kind: CALLS 3,254,
REFERENCES 2,020, TESTS 1,824, IMPORTS 1,104, DECORATES 847, OVERRIDES
317, IMPLEMENTS 168, INHERITS 16. By resolution strategy:
`import_resolved` 6,677, `type_resolved` 2,865, `same_module` 8.

### Contracts

`rueca contracts --org tokio-rs` lists 777 rows:

| Kind | Side | Framework | Rows |
|---|---|---|---:|
| http | provides | axum | 361 |
| http | consumes | reqwest | 73 |
| rpc | provides | proto | 137 |
| rpc | provides | tonic | 99 |
| rpc | consumes | tonic | 107 |

The tonic providers are `impl Greeter for MyGreeter` and similar; the
tonic consumers are calls through generated clients such as
`GreeterClient`. 500 rows have at least one counterpart. tonic rows carry
`Greeter/SayHello` and `.proto` rows `helloworld.Greeter/SayHello`; an RPC
key without a package matches one with it (RFC 0006 §9.4), so they count
as counterparts. 19 consumers are dangling, and all 19 are false
positives (see [below](#what-does-not-work-yet-on-this-organization)).

## Showcase queries

Every query below runs against the organization with `--text`, which
prints what an agent reads over MCP. The latency is the median of three
runs, CLI process start included; every query took 23 to 57 ms warm. The
first call right after an index took 59 and 65 ms in two tries.

How to read the output, once:

- `[0]`, `[1]` …: when an answer spans several snapshots, the header lists
  each one (`repo@commit (ref)`) with an index, and every item or node is
  prefixed with the index of its snapshot.
- `in d1`: in a `trace`, a caller (`in`) or callee (`out`) at depth 1.
- `→0 CALLS 0.85 type_resolved @199`: this node calls node 0; edge kind,
  confidence, how the call was resolved, and the line of the call site.
  `←0` is the other direction: node 0 calls this node.
- ` [0,1]` at the end of an item: identical in snapshots 0 and 1.
  ` changed`: not identical in every requested ref.
- `(truncated; continue with "cursor": "8")`: the limit or the token budget
  cut the answer; pass the cursor to continue.

### 1. Which repositories is a question about

```sh
rueca query list_repositories '{"query":"http router"}' --org tokio-rs --text
```

I cut the long `packages` lists (the ones that end in `(+N)`) to
`packages […]`; the short ones are as captured.

```
[0] github.com/hyperium/hyper@7f67699ce32d (HEAD)
[1] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[2] github.com/tokio-rs/axum@c59208c86fde (HEAD)
[3] github.com/tokio-rs/bytes@67edc2b999d6 (HEAD)
[4] github.com/tokio-rs/mio@101e5b7c92b1 (HEAD)
[5] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
[6] github.com/tokio-rs/tracing@2d55f6faf9be (HEAD)
[7] github.com/tower-rs/tower@4b0a6b0e688b (HEAD)
[2] github.com/tokio-rs/axum  refs HEAD@c59208c86fde  488 files, 5676 symbols, 17387 edges  rust 296, toml 63, markdown 40, javascript 5, yaml 3, json 1  packages […]  depends on github.com/hyperium/hyper, github.com/tokio-rs/bytes, github.com/tokio-rs/tokio, github.com/tokio-rs/tracing, github.com/tower-rs/tower  (1.0)
[0] github.com/hyperium/hyper  refs HEAD@7f67699ce32d  139 files, 4042 symbols, 11226 edges  rust 99, markdown 23, yaml 5, c 3, toml 3  packages , hyper  depends on github.com/tokio-rs/bytes, github.com/tokio-rs/tokio, github.com/tokio-rs/tracing  (0.402)
[1] github.com/hyperium/tonic  refs HEAD@4c0f455c264f  815 files, 18813 symbols, 52822 edges  rust 467, proto 203, toml 38, markdown 37, yaml 4, cpp 3, hcl 2, json 2, python 1  packages […]  depends on github.com/hyperium/hyper, github.com/tokio-rs/axum, github.com/tokio-rs/bytes, github.com/tokio-rs/tokio, github.com/tokio-rs/tracing, github.com/tower-rs/tower  (0.314)
[7] github.com/tower-rs/tower  refs HEAD@4b0a6b0e688b  162 files, 1807 symbols, 5477 edges  rust 133, markdown 11, toml 9, yaml 3  packages examples, tower, tower-layer, tower-service, tower-test  depends on github.com/tokio-rs/tokio, github.com/tokio-rs/tracing  (0.0479)
[6] github.com/tokio-rs/tracing  refs HEAD@2d55f6faf9be  345 files, 6473 symbols, 24007 edges  rust 260, markdown 31, toml 23, yaml 4  packages […]  depends on github.com/hyperium/hyper, github.com/tokio-rs/bytes, github.com/tokio-rs/mio, github.com/tokio-rs/tokio, github.com/tower-rs/tower  (0.0118)
[5] github.com/tokio-rs/tokio  refs HEAD@b53b2001ed2c, tokio-1.53.3@a85c3f5baa29  886 files, 15243 symbols, 56615 edges  rust 811, markdown 29, toml 18, yaml 11, json 1  packages […]  depends on github.com/tokio-rs/bytes, github.com/tokio-rs/mio, github.com/tokio-rs/tracing  (0.00295)
[3] github.com/tokio-rs/bytes  refs HEAD@67edc2b999d6  47 files, 1192 symbols, 3726 edges  rust 35, markdown 3, toml 2, yaml 1  packages bytes  (0.00137)
[4] github.com/tokio-rs/mio  refs HEAD@101e5b7c92b1  89 files, 2038 symbols, 7589 edges  rust 82, markdown 2, toml 1, yaml 1  packages mio  (0.0)
```

Median 52 ms. Why it matters for an agent: one call names the repository
to read first (axum), its refs, and what it depends on, before any symbol
search.

### 2. One ranking over eight repositories

```sh
rueca query search_symbols '{"query":"graceful shutdown","limit":8}' --org tokio-rs --text
```

```
[0] github.com/hyperium/hyper@7f67699ce32d (HEAD)
[1] github.com/tokio-rs/axum@c59208c86fde (HEAD)
[1] axum/src/serve/mod.rs
  Method Serve.with_graceful_shutdown L151-161  pub fn with_graceful_shutdown<F>(self, signal: F) -> WithGracefulShutdown<L, M, S, F> where F: Future<Output = ()> + Send + 'static,
  Struct WithGracefulShutdown L238-243  pub struct WithGracefulShutdown<L, M, S, F>
  Method WithGracefulShutdown.fmt L315-328  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
[1] examples/tls-graceful-shutdown/src/main.rs
  Function shutdown_signal L72-98  async fn shutdown_signal(handle: axum_server::Handle)
[1] examples/graceful-shutdown/src/main.rs
  Function main L19-53  async fn main()
  Function shutdown_signal L55-77  async fn shutdown_signal()
[0] src/server/conn/http2.rs
  Method Connection.graceful_shutdown L78-80  pub fn graceful_shutdown(mut self: Pin<&mut Self>)
[0] src/proto/h2/server.rs
  Method Server.graceful_shutdown L185-197  pub(crate) fn graceful_shutdown(&mut self)
(truncated; continue with "cursor": "8")
```

Median 53 ms. Why it matters for an agent: scores are comparable across
repositories, so axum's serve API and hyper's HTTP/2 connection code come
back in one list instead of one list per repository.

### 3. Narrowing the same ranking with `repo`

```sh
rueca query search_symbols '{"query":"decode frame","limit":8}' --org tokio-rs --text
```

```
github.com/hyperium/hyper@7f67699ce32d (HEAD)
src/proto/h1/decode.rs
  Method Decoder.decode_fut L241-243  async fn decode_fut<R: MemRead>(&mut self, body: &mut R) -> Result<Frame<Bytes>, io::Error>
  Method Decoder.decode L144-238  pub(crate) fn decode<R: MemRead>( &mut self, cx: &mut Context<'_>, body: &mut R, ) -> Poll<Result<Frame<Bytes>, io::Error>>
examples/single_threaded.rs
  Method Body.poll_frame L54-59  fn poll_frame( self: Pin<&mut Self>, _: &mut Context<'_>, ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>>
src/client/conn/http2.rs
  Method Builder.max_frame_size L391-394  pub fn max_frame_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self
src/server/conn/http2.rs
  Method Builder.max_frame_size L199-204  pub fn max_frame_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self
src/proto/h2/mod.rs
  Method SendStreamExt.send_eos_frame L266  fn send_eos_frame(&mut self) -> crate::Result<()>
  Method SendStream.send_eos_frame L280-284  fn send_eos_frame(&mut self) -> crate::Result<()>
src/proto/h1/conn.rs
  Method Conn.poll_read_body L365-429  pub(crate) fn poll_read_body( &mut self, cx: &mut Context<'_>, ) -> Poll<Option<io::Result<Frame<Bytes>>>>
(truncated; continue with "cursor": "8")
```

```sh
rueca query search_symbols '{"query":"decode frame","repo":"github.com/tokio-rs/tokio","limit":6}' --org tokio-rs --text
```

```
github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
tokio-util/src/codec/decoder.rs
  Method Decoder.decode_eof L144-155  fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error>
tokio-util/src/codec/length_delimited.rs
  Method LengthDelimitedCodec.decode_eof L601-609  fn decode_eof(&mut self, src: &mut BytesMut) -> io::Result<Option<BytesMut>>
tokio-util/src/codec/any_delimiter_codec.rs
  Method AnyDelimiterCodec.decode_eof L194-208  fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Bytes>, AnyDelimiterCodecError>
tokio/src/runtime/task/trace/mod.rs
  Struct Frame L42-50  struct Frame
tokio-util/src/codec/lines_codec.rs
  Method LinesCodec.decode_eof L165-181  fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<String>, LinesCodecError>
tokio/src/runtime/dump.rs
  Method BacktraceFrame.from_resolved_backtrace_frame L119-129  pub(crate) fn from_resolved_backtrace_frame(frame: &backtrace::BacktraceFrame) -> Self
(truncated; continue with "cursor": "6")
```

```sh
rueca query search_symbols '{"query":"decode frame","repo":["github.com/hyperium/tonic","github.com/tokio-rs/tokio"],"limit":8}' --org tokio-rs --text
```

```
github.com/hyperium/tonic@4c0f455c264f (HEAD)
tonic-web/src/call.rs
  Method GrpcWebCall.poll_decode L194-238  fn poll_decode( mut self: Pin<&mut Self>, cx: &mut Context<'_>, ) -> Poll<Option<Result<Frame<Bytes>, Status>>>
  Method GrpcWebCall.poll_frame L282-411  fn poll_frame( mut self: Pin<&mut Self>, cx: &mut Context<'_>, ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>>
tonic/src/codec/decode.rs
  Method StreamingInner.poll_frame L281-318  fn poll_frame(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<()>, Status>>
  Method Streaming.decode_chunk L405-419  fn decode_chunk(&mut self) -> Result<Option<T>, Status>
xds-client/src/resource/mod.rs
  Function decode L194-206  pub(crate) fn decode<T: Resource>(bytes: Bytes) -> DecodeResult<T>
tonic/src/codec/mod.rs
  Method Decoder.decode L178  fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error>
  TypeAlias Codec.Decode L133  type Decode: Send + 'static
tonic-prost/src/codec.rs
  Method ProstDecoder.decode L166-172  fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error>
(truncated; continue with "cursor": "8")
```

Medians 53, 24 and 38 ms. Why it matters for an agent: the same words
mean different code in each repository, and `repo` (one id or a list)
picks which part of the one ranking to read. With the list, all eight
hits come from tonic; tokio's codec methods rank lower in that ranking.

### 4. Who drives mio's event loop

```sh
rueca query trace '{"qualified_name":"mio::poll::Poll.poll","direction":"callers","depth":4}' --org tokio-rs --text
```

```
[0] github.com/tokio-rs/mio@101e5b7c92b1 (HEAD)
[1] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
0 [0] Method Poll.poll  src/poll.rs L438-440
1 [1] in d1 Method Driver.turn  tokio/src/runtime/io/driver.rs L185-258  →0 CALLS 0.85 type_resolved @199
2 [1] in d2 Method Driver.park  tokio/src/runtime/io/driver.rs L165-168  →1 CALLS 0.9 type_resolved @167
3 [1] in d2 Method Driver.park_timeout  tokio/src/runtime/io/driver.rs L170-173  →1 CALLS 0.9 type_resolved @172
4 [1] in d3 Method Driver.park  tokio/src/runtime/signal/mod.rs L91-94  →2 CALLS 0.85 type_resolved @92
5 [1] in d3 Method Driver.park_timeout  tokio/src/runtime/signal/mod.rs L96-99  →3 CALLS 0.85 type_resolved @97
6 [1] in d4 Method Driver.park  tokio/src/runtime/process.rs L31-34  →4 CALLS 0.85 type_resolved @32
7 [1] in d4 Method Driver.park_timeout  tokio/src/runtime/process.rs L36-39  →5 CALLS 0.85 type_resolved @37
(9 tests excluded; `include_tests: true` lists them)
```

Median 54 ms. The walk crosses from mio (`[0]`) into tokio (`[1]`) at
depth 1 and then climbs the stack of drivers: the I/O driver's `turn`,
its `park` and `park_timeout`, the signal driver that wraps it, and the
process driver that wraps that. Every edge says how it was resolved and
on which line the call is (`@199` in `driver.rs`). Why it matters for an
agent: the question spans two repositories and four levels, and the
answer is one call with the provenance of each step.

### 5. Callers of a bytes method across the organization

```sh
rueca query trace '{"qualified_name":"bytes::bytes::Bytes.copy_from_slice","direction":"callers","limit":15}' --org tokio-rs --text
```

```
[0] github.com/hyperium/hyper@7f67699ce32d (HEAD)
[1] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[2] github.com/tokio-rs/axum@c59208c86fde (HEAD)
[3] github.com/tokio-rs/bytes@67edc2b999d6 (HEAD)
[4] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
0 [3] Method Bytes.copy_from_slice  src/bytes.rs L347-349
1 [2] in d1 Method RawForm.from_request  axum/src/extract/raw_form.rs L38-52  →0 CALLS 0.85 import_resolved @41
2 [2] in d1 Method Message.from  axum/src/extract/ws.rs L899-901  →0 CALLS 0.85 import_resolved @912
3 [4] in d1 Function recv  examples/connect-udp.rs L82-94  →0 CALLS 0.85 import_resolved @91
4 [1] in d1 Function do_connect_handshake  grpc/src/client/transport/http_connect/mod.rs L54-136  →0 CALLS 0.85 import_resolved @99
5 [1] in d1 Method Ascii.decode  grpc/src/metadata/encoding.rs L178-185  →0 CALLS 0.85 import_resolved @184
6 [1] in d1 Method Ascii.from_bytes  grpc/src/metadata/encoding.rs L133-145  →0 CALLS 0.85 import_resolved @142
7 [1] in d1 Method Binary.from_bytes  grpc/src/metadata/encoding.rs L248-255  →0 CALLS 0.85 import_resolved @252
8 [0] in d1 Method ReasonPhrase.try_from  src/ext/h1_reason_phrase.rs L70-76  →0 CALLS 0.85 import_resolved @74
9 [0] in d1 Function raw_name_value  src/ffi/http_types.rs L559-578  →0 CALLS 0.85 import_resolved @566
10 [0] in d1 Method Client.parse  src/proto/h1/role.rs L1037-1213  →0 CALLS 0.85 import_resolved @1075
11 [0] in d1 File src/ffi/body.rs L1-302  →0 CALLS 0.85 import_resolved @265
12 [4] in d1 Method CopyToBytes.start_send  tokio-util/src/io/copy_to_bytes.rs L56-60  →0 CALLS 0.85 import_resolved @59
13 [1] in d1 Method Ascii.decode  tonic/src/metadata/encoding.rs L121-123  →0 CALLS 0.85 import_resolved @122
14 [1] in d1 Method Bytes.from  tonic/src/metadata/key.rs L219-221  →0 CALLS 0.85 import_resolved @220
15 [1] in d1 Method Bytes.from  tonic/src/metadata/value.rs L610-612  →0 CALLS 0.85 import_resolved @611
(22 tests excluded; `include_tests: true` lists them)
(truncated; continue with "cursor": "15")
```

Median 54 ms. A `File` caller (node 11) is module-level code. Why it
matters for an agent: before changing a function in a shared crate, this
is the list of call sites in the four repositories that use it, tests
counted separately.

### 6. How graceful shutdown is wired: who creates watch channels

```sh
rueca query trace '{"qualified_name":"tokio::sync::watch::channel","direction":"callers","limit":12}' --org tokio-rs --text
```

```
[0] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[1] github.com/tokio-rs/axum@c59208c86fde (HEAD)
[2] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
0 [2] Function channel  tokio/src/sync/watch.rs L554-574
1 [1] in d1 Method Serve.run  axum/src/serve/mod.rs L179-193  →0 CALLS 0.9 import_resolved @186
2 [1] in d1 Method WithGracefulShutdown.run  axum/src/serve/mod.rs L267-304  →0 CALLS 0.9 import_resolved @275
3 [0] in d1 Method GracefulCoordinator.new  grpc/src/server/mod.rs L166-169  →0 CALLS 0.9 import_resolved @167
4 [0] in d1 Method BenchmarkClient.start  grpc-benchmark/src/client.rs L73-222  →0 CALLS 0.9 import_resolved @178
5 [0] in d1 Method StaticProvider.get_receiver  grpc-rustls/src/lib.rs L125-131  →0 CALLS 0.9 import_resolved @129
6 [2] in d1 Method EventInfo.default  tokio/src/signal/registry.rs L19-26  →0 CALLS 0.9 import_resolved @20
7 [2] in d1 Method Barrier.new  tokio/src/sync/barrier.rs L63-111  →0 CALLS 0.9 import_resolved @64
8 [2] in d1 Method Sender.new  tokio/src/sync/watch.rs L1059-1062  →0 CALLS 0.9 same_module @1060
9 [0] in d1 Method Server.serve_internal  tonic/src/transport/server/mod.rs L774-918  →0 CALLS 0.9 import_resolved @858
10 [0] in d1 Method HealthReporter.new  tonic-health/src/server.rs L65-72  →0 CALLS 0.9 import_resolved @67
11 [0] in d1 Method HealthReporter.set_service_status  tonic-health/src/server.rs L98-116  →0 CALLS 0.9 import_resolved @113
12 [0] in d1 Method OutlierStatsRegistry.new  tonic-xds/src/client/loadbalance/outlier_detection.rs L105-118  →0 CALLS 0.9 import_resolved @108
(42 tests excluded; `include_tests: true` lists them)
(truncated; continue with "cursor": "12")
```

Median 54 ms. Both servers create tokio watch channels in their serve
loops: axum in `Serve.run` and `WithGracefulShutdown.run`, tonic in
`Server.serve_internal`; tonic's `grpc` crate does it in
`GracefulCoordinator.new`. Why it matters for an agent: "how does X work
in both frameworks" becomes one trace from the shared primitive, instead
of two searches that may use different words.

### 7. What the tokio I/O driver calls

```sh
rueca query trace '{"qualified_name":"tokio::runtime::io::driver::Driver.turn","direction":"callees"}' --org tokio-rs --text
```

```
[0] github.com/tokio-rs/mio@101e5b7c92b1 (HEAD)
[1] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
0 [1] Method Driver.turn  tokio/src/runtime/io/driver.rs L185-258
1 [0] out d1 Method Poll.poll  src/poll.rs L438-440  ←0 CALLS 0.85 type_resolved @199
2 [1] out d1 Method Ready.from_mio  tokio/src/io/ready.rs L57-99  ←0 CALLS 0.85 import_resolved @220
3 [1] out d1 Method Mutex.lock  tokio/src/loom/std/parking_lot.rs L61-63  ←0 CALLS 0.85 type_resolved @186
4 [1] out d1 Method Handle.release_pending_registrations  tokio/src/runtime/io/driver.rs L339-343  ←0 CALLS 0.85 type_resolved @188
5 [1] out d1 Function dispatch_event  tokio/src/runtime/io/driver.rs L353-361  ←0 CALLS 0.9 same_module @226
6 [1] out d1 Method Handle.get_uring  tokio/src/runtime/io/driver/uring.rs L200-202  ←0 CALLS 0.85 type_resolved @240
7 [1] out d1 Method RegistrationSet.is_shutdown  tokio/src/runtime/io/registration_set.rs L47-49  ←0 CALLS 0.85 type_resolved @186
```

Median 55 ms. The same edge as in query 4, seen from the other end
(`←0 … @199`). Why it matters for an agent: the callees of one function,
each with its call-site line, outline what it does before reading its 74
lines.

### 8. `repo` picks the root; the callers come from everywhere

```sh
rueca query trace '{"qualified_name":"ServiceBuilder.layer","repo":"github.com/tower-rs/tower","direction":"callers","min_confidence":0.85,"limit":10}' --org tokio-rs --text
```

```
[0] github.com/tower-rs/tower@4b0a6b0e688b (HEAD)
[1] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[2] github.com/tokio-rs/axum@c59208c86fde (HEAD)
0 [0] Method ServiceBuilder.layer  tower/src/builder/mod.rs L132-136
1 [2] in d1 Function app  examples/compression/src/main.rs L29-35  →0 CALLS 0.85 type_resolved @32
2 [2] in d1 Function main  examples/key-value-store/src/main.rs L33-82  →0 CALLS 0.85 type_resolved @68
3 [2] in d1 Function main  examples/request-id/src/main.rs L24-76  →0 CALLS 0.85 type_resolved @43
4 [2] in d1 Function main  examples/todos/src/main.rs L36-76  →0 CALLS 0.85 type_resolved @55
5 [1] in d1 Function main  examples/src/grpc-web/client.rs L34-53  →0 CALLS 0.85 type_resolved @39
6 [1] in d1 Function main  examples/src/grpc-web/server.rs L53-75  →0 CALLS 0.85 type_resolved @60
7 [1] in d1 Method MakeSvc.call  tonic/src/transport/server/mod.rs L1269-1293  →0 CALLS 0.85 type_resolved @1278
8 [1] in d1 Method Server.layer  tonic/src/transport/server/mod.rs L648-675  →0 CALLS 0.85 type_resolved @650
9 [1] in d1 Method XdsChannelBuilder.assemble_channel_stack  tonic-xds/src/client/channel.rs L660-690  →0 CALLS 0.85 type_resolved @680
10 [0] in d1 Method ServiceBuilder.and_then  tower/src/builder/mod.rs L459-461  →0 CALLS 0.9 type_resolved @460
(17 tests excluded; `include_tests: true` lists them)
(truncated; continue with "cursor": "10")
```

Median 38 ms. A short name can match symbols in several repositories;
`repo` says which one is meant, and the walk still
follows link edges into axum and tonic. `min_confidence` drops
low-confidence edges. Why it matters for an agent: a short name plus a
repository is enough to start a cross-repository trace, without knowing
the full module path.

### 9. Who spawns tokio tasks

```sh
rueca query trace '{"qualified_name":"tokio::task::spawn::spawn","direction":"callers","limit":15}' --org tokio-rs --text
```

```
[0] github.com/hyperium/hyper@7f67699ce32d (HEAD)
[1] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[2] github.com/tokio-rs/axum@c59208c86fde (HEAD)
[3] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
[4] github.com/tokio-rs/tracing@2d55f6faf9be (HEAD)
0 [3] Function spawn  tokio/src/task/spawn.rs L174-185
1 [2] in d1 Method WebSocketUpgrade.on_upgrade  axum/src/extract/ws.rs L347-411  →0 CALLS 0.9 import_resolved @359
2 [2] in d1 Method WithGracefulShutdown.run  axum/src/serve/mod.rs L267-304  →0 CALLS 0.9 import_resolved @276
3 [2] in d1 Function handle_connection  axum/src/serve/mod.rs L353-416  →0 CALLS 0.9 import_resolved @389
4 [2] in d1 Function spawn_service  axum/src/test_helpers/test_client.rs L11-30  →0 CALLS 0.9 import_resolved @23
5 [1] in d1 Function main  xds-client/examples/basic.rs L160-244  →0 CALLS 0.9 import_resolved @197
6 [1] in d1 Function main  grpc-benchmark/src/bin/bench.rs L135-336  →0 CALLS 0.9 import_resolved @176
7 [1] in d1 Function with_cancellation_handler  examples/src/cancellation/server.rs L73-94  →0 CALLS 0.9 import_resolved @84
8 [0] in d1 Function fetch_url  examples/client.rs L42-81  →0 CALLS 0.9 import_resolved @50
9 [0] in d1 Function fetch_json  examples/client_json.rs L30-62  →0 CALLS 0.9 import_resolved @39
10 [1] in d1 Function main  examples/src/dynamic_load_balance/client.rs L40-104  →0 CALLS 0.9 import_resolved @49
11 [1] in d1 Function main  examples/src/dynamic_load_balance/server.rs L52-78  →0 CALLS 0.9 import_resolved @66
12 [0] in d1 Function main  examples/echo.rs L92-110  →0 CALLS 0.9 import_resolved @101
13 [4] in d1 Function main  examples/examples/echo.rs L37-122  →0 CALLS 0.9 import_resolved @73
14 [2] in d1 Function websocket  examples/chat/src/main.rs L72-146  →0 CALLS 0.9 import_resolved @111
15 [2] in d1 Function main  examples/websockets/src/client.rs L29-46  →0 CALLS 0.9 import_resolved @33
(422 tests excluded; `include_tests: true` lists them)
(truncated; continue with "cursor": "15")
```

Median 54 ms. tokio declares `spawn` inside a `cfg_rt! { … }` body;
before `4905a37` it was not in the index at all. Now it has 92 callers
outside tests (514 with them) in six repositories: axum 19, hyper 19,
tokio 15, tonic 31, tower 2 and tracing 6 (`out/checks.txt`). The first
fifteen are axum's serve loop, WebSocket upgrade and examples, hyper's
examples, tonic's examples and benchmark, and a tracing example. The name
to ask for is the file's module path, `tokio::task::spawn::spawn`; the
re-export path `tokio::spawn` is not a name yet (see below). Why it
matters for an agent: "who starts background tasks, and where" is one call
across the organization.

### 10. Every function `#[tokio::main]` decorates

```sh
rueca query trace '{"qualified_name":"tokio_macros::main","direction":"callees","edge_types":["DECORATES"],"limit":10}' --org tokio-rs --text
```

```
[0] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[1] github.com/tokio-rs/axum@c59208c86fde (HEAD)
[2] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
[3] github.com/tokio-rs/tracing@2d55f6faf9be (HEAD)
0 [2] Function main  tokio-macros/src/lib.rs L314-316
1 [3] out d1 Function main  examples/examples/async-fn.rs L42-57  ←0 DECORATES 0.9 import_resolved @41 examples/examples/async-fn.rs
2 [0] out d1 Function main  examples/src/authentication/client.rs L33-52  ←0 DECORATES 0.9 import_resolved @32 examples/src/authentication/client.rs
3 [0] out d1 Function main  examples/src/authentication/server.rs L46-55  ←0 DECORATES 0.9 import_resolved @45 examples/src/authentication/server.rs
4 [1] out d1 Function main  examples/auto-reload/src/main.rs L12-30  ←0 DECORATES 0.9 import_resolved @11 examples/auto-reload/src/main.rs
5 [0] out d1 Function main  examples/src/autoreload/server.rs L53-76  ←0 DECORATES 0.9 import_resolved @52 examples/src/autoreload/server.rs
6 [0] out d1 Function main  xds-client/examples/basic.rs L160-244  ←0 DECORATES 0.9 import_resolved @159 xds-client/examples/basic.rs
7 [0] out d1 Function main  grpc-benchmark/src/bin/bench.rs L135-336  ←0 DECORATES 0.9 import_resolved @134 grpc-benchmark/src/bin/bench.rs
8 [0] out d1 Function main  examples/src/cancellation/client.rs L35-54  ←0 DECORATES 0.9 import_resolved @34 examples/src/cancellation/client.rs
9 [0] out d1 Function main  examples/src/cancellation/server.rs L97-109  ←0 DECORATES 0.9 import_resolved @96 examples/src/cancellation/server.rs
10 [0] out d1 Function main  tonic-xds/examples/channel.rs L63-93  ←0 DECORATES 0.9 import_resolved @62 tonic-xds/examples/channel.rs
(37 tests excluded; `include_tests: true` lists them)
(truncated; continue with "cursor": "10")
```

Median 55 ms. An attribute macro is a decorator: a `DECORATES` edge
runs from the `#[proc_macro_attribute] pub fn main` in tokio-macros to
each function it decorates, so the walk goes out from the macro
(`callees`). On these link edges the file after the line (`@41
examples/examples/async-fn.rs`) is where the attribute is written, in the
consumer, not in tokio-macros. Across the five consumers there are 146
such edges for `#[tokio::main]` and 699 for `#[tokio::test]` (from
`tokio_macros::test`). Functions in test code are left out, as the last
line says. Why it matters for an agent: "which programs start a tokio
runtime, and which tests run on one" is one call per macro.

### 11. Two releases of tokio side by side

```sh
rueca query search_symbols '{"query":"blocking acquire lock","repo":"github.com/tokio-rs/tokio","ref":["HEAD","tokio-1.53.3"],"limit":8}' --org tokio-rs --text
```

```
[0] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
[1] github.com/tokio-rs/tokio@a85c3f5baa29 (tokio-1.53.3)
[0] tokio/src/sync/semaphore.rs
  Method Semaphore.blocking_acquire L802-804  pub fn blocking_acquire(&self) -> Result<SemaphorePermit<'_>, AcquireError> changed
  Method Semaphore.blocking_acquire_owned L1086-1088  pub fn blocking_acquire_owned(self: Arc<Self>) -> Result<OwnedSemaphorePermit, AcquireError> changed
  Method Semaphore.blocking_acquire_many L838-840  pub fn blocking_acquire_many(&self, n: u32) -> Result<SemaphorePermit<'_>, AcquireError> changed
  Method Semaphore.blocking_acquire_many_owned L1125-1130  pub fn blocking_acquire_many_owned( self: Arc<Self>, n: u32, ) -> Result<OwnedSemaphorePermit, AcquireError> changed
[0] tokio/src/sync/mutex.rs
  Method Mutex.blocking_lock L520-522  pub fn blocking_lock(&self) -> MutexGuard<'_, T> [0,1]
  Method Mutex.blocking_lock_owned L578-580  pub fn blocking_lock_owned(self: Arc<Self>) -> OwnedMutexGuard<T> [0,1]
  Method Mutex.lock L434-466  pub async fn lock(&self) -> MutexGuard<'_, T> [0,1]
[0] tokio/src/sync/rwlock.rs
  Method RwLock.blocking_write L877-879  pub fn blocking_write(&self) -> RwLockWriteGuard<'_, T> changed
(truncated; continue with "cursor": "8")
```

```sh
rueca query search_symbols '{"query":"blocking acquire lock","repo":"github.com/tokio-rs/tokio","ref":"tokio-1.53.3","limit":5}' --org tokio-rs --text
```

```
github.com/tokio-rs/tokio@a85c3f5baa29 (tokio-1.53.3)
tokio/src/sync/mutex.rs
  Method Mutex.blocking_lock L520-522  pub fn blocking_lock(&self) -> MutexGuard<'_, T>
  Method Mutex.blocking_lock_owned L578-580  pub fn blocking_lock_owned(self: Arc<Self>) -> OwnedMutexGuard<T>
  Method Mutex.lock L434-466  pub async fn lock(&self) -> MutexGuard<'_, T>
tokio/src/sync/rwlock.rs
  Method RwLock.blocking_write L877-879  pub fn blocking_write(&self) -> RwLockWriteGuard<'_, T>
  Method RwLock.blocking_read L529-531  pub fn blocking_read(&self) -> RwLockReadGuard<'_, T>
(truncated; continue with "cursor": "5")
```

Medians 34 and 23 ms. With a `ref` list, a symbol that is identical in
both releases comes back once, marked ` [0,1]` (the `Mutex` methods); a
symbol whose version differs is marked ` changed`. The comparison is per
file blob, not per symbol: `semaphore.rs` changed between the releases
(new methods, `permits` from `u32` to `usize`), so every `Semaphore`
method is `changed`, and `rwlock.rs` changed only in two doc-test lines
of other methods, yet `RwLock.blocking_write` is `changed` too. Read
` changed` as "its file changed"; ` [0,1]` is exact. The second query
reads the older release alone, with no `changed` marks. Why it matters
for an agent: "what is different in this release around X" is one call,
and ` [0,1]` rules out the code that did not move.

### 12. gRPC contracts

```sh
rueca query list_contracts '{"kind":"rpc","query":"Greeter/SayHello","limit":12}' --org tokio-rs --text
```

```
github.com/hyperium/tonic@4c0f455c264f (HEAD)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/autoreload/server.rs#MyGreeter.say_hello L39  (24)
Greeter/SayHello  consumes  tonic  github.com/hyperium/tonic:examples/src/blocking/client.rs#BlockingClient.say_hello L61  (26)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/blocking/server.rs#MyGreeter.say_hello L41  (24)
Greeter/SayHello  consumes  tonic  github.com/hyperium/tonic:examples/src/cancellation/client.rs#main L43  (26)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/cancellation/server.rs#MyGreeter.say_hello L46  (24)
Greeter/SayHello  consumes  tonic  github.com/hyperium/tonic:examples/src/codec_buffers/client.rs#main L49  (26)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/codec_buffers/server.rs#MyGreeter.say_hello L49  (24)
Greeter/SayHello  consumes  tonic  github.com/hyperium/tonic:examples/src/compression/client.rs#main L49  (26)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/compression/server.rs#MyGreeter.say_hello L40  (24)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/dynamic/server.rs#MyGreeter.say_hello L72  (24)
Greeter/SayHello  consumes  tonic  github.com/hyperium/tonic:examples/src/grpc-web/client.rs#main L48  (26)
Greeter/SayHello  provides  tonic  github.com/hyperium/tonic:examples/src/grpc-web/server.rs#MyGreeter.say_hello L39  (24)
(truncated; continue with "cursor": "12")
```

```sh
rueca query list_contracts '{"side":"dangling","limit":6}' --org tokio-rs --text
```

```
github.com/hyperium/tonic@4c0f455c264f (HEAD)
Blocking/SayHello  consumes  tonic  github.com/hyperium/tonic:examples/src/blocking/client.rs#main L72  (0)
Echo/Unwrap  consumes  tonic  github.com/hyperium/tonic:examples/src/streaming/client.rs#main L92  (0)
Test/Unwrap  consumes  tonic  github.com/hyperium/tonic:tests/default_stubs/tests/default.rs#test_default_stubs_tcp L103  (0)
Test/Unwrap  consumes  tonic  github.com/hyperium/tonic:tests/default_stubs/tests/default.rs#test_default_stubs_uds L116  (0)
Test/Unwrap  consumes  tonic  github.com/hyperium/tonic:tests/integration_tests/tests/connection.rs#connect_handles_tls L62  (0)
Test/Unwrap  consumes  tonic  github.com/hyperium/tonic:tests/integration_tests/tests/connection.rs#connect_returns_err_via_call_after_connected L85  (0)
(truncated; continue with "cursor": "6")
```

Medians 52 and 53 ms. Each row is a key, a side, the framework that
declared it, the handler or call site, and in parentheses its number of
counterparts: every `MyGreeter.say_hello` serves the 24 client calls, and
every client call reaches 26 providers (23 tonic servers and 3 `.proto`
declarations). The dangling list shows the current false positives: an
`.unwrap()` after a client call read as an RPC named `Unwrap`, and a
hand-written `BlockingClient` read as a stub. Why it matters for an
agent: "who serves this RPC, who calls it" across code and `.proto`
files is one call; the dangling list is where a broken contract would
appear, once the false positives are gone.

### 13. A file outline

```sh
rueca query get_file_outline '{"path":"tokio/src/runtime/io/driver.rs","repo":"github.com/tokio-rs/tokio"}' --org tokio-rs --text
```

The first 16 lines; the remaining 28 (the other structs, enums,
constants, the methods of `Driver` and `Handle`, and one test) are in
`out/20-outline-io-driver.txt`.

```
github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
tokio/src/runtime/io/driver.rs (rust, 407 lines)
  Struct Driver L26-39  pub(crate) struct Driver
    Field Driver.signal_ready L28  signal_ready: bool
    Field Driver.events L31  events: mio::Events
    Field Driver.events_busy L35  events_busy: Option<mio::Events>
    Field Driver.poll L38  poll: mio::Poll
  Struct Handle L42-76  pub(crate) struct Handle
    Field Handle.registry L44  registry: mio::Registry
    Field Handle.registrations L47  registrations: RegistrationSet
    Field Handle.synced L50  synced: Mutex<registration_set::Synced>
    Field Handle.waker L55  waker: mio::Waker
    Field Handle.metrics L57  pub(crate) metrics: IoDriverMetrics
    Field Handle.uring_context L66  pub(crate) uring_context: Mutex<UringContext>
    Field Handle.uring_probe L75  pub(crate) uring_probe: OnceCell<Option<io_uring::Probe>>
  Struct ReadyEvent L79-83  pub(crate) struct ReadyEvent
[…]
```

Median 23 ms. Why it matters for an agent: the shape of a 407-line file
(and which fields hold mio types) costs a few hundred tokens instead of
the file.

### 14. Text search across repositories

```sh
rueca query search_code '{"pattern":"cfg_net!\\s*\\{","regex":true,"language":"rust","limit":8}' --org tokio-rs --text
```

```
[0] github.com/tokio-rs/mio@101e5b7c92b1 (HEAD)
[1] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
[0] src/lib.rs
  66: cfg_net! {
[0] src/sys/shell/mod.rs
  15: cfg_net! {
[0] src/sys/unix/mod.rs
  133:     cfg_net! {
[0] src/sys/wasip1/mod.rs
  26: cfg_net! {
[0] src/sys/windows/mod.rs
  19: cfg_net! {
[0] src/sys/windows/selector.rs
  6: cfg_net! {
  727: cfg_net! {
[1] tokio-stream/src/wrappers.rs
  46: cfg_net! {
(truncated; continue with "cursor": "8")
```

Median 55 ms. `search_code` is a regex over file contents, in repository
order. Why it matters for an agent: literals and macro invocations, which
the symbol graph does not hold as symbols, are still reachable. These
`cfg_net!` bodies are where `TcpListener` and `UdpSocket` are declared;
the graph now holds the items inside them (query 9).

### 15. A token budget

```sh
rueca query trace '{"qualified_name":"tokio::sync::oneshot::channel","direction":"callers","budget_tokens":400}' --org tokio-rs --text
```

```
[0] github.com/hyperium/hyper@7f67699ce32d (HEAD)
[1] github.com/hyperium/tonic@4c0f455c264f (HEAD)
[2] github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
[3] github.com/tower-rs/tower@4b0a6b0e688b (HEAD)
0 [2] Function channel  tokio/src/sync/oneshot.rs L497-579
1 [2] in d1 Function main  examples/custom-executor-tokio-context.rs L11-32  →0 CALLS 0.9 import_resolved @12
2 [2] in d1 Function main  examples/custom-executor.rs L10-22  →0 CALLS 0.9 import_resolved @11
3 [1] in d1 Method CaptureHeadersInterceptor.new  grpc/src/client/metadata_utils.rs L88-91  →0 CALLS 0.9 import_resolved @89
4 [1] in d1 Method CaptureTrailersInterceptor.new  grpc/src/client/metadata_utils.rs L144-147  →0 CALLS 0.9 import_resolved @145
5 [1] in d1 Method TonicTransport.invoke  grpc/src/client/transport/tonic/mod.rs L153-199  →0 CALLS 0.9 import_resolved @185
6 [1] in d1 Method TransportBuilder.connect  grpc/src/client/transport/tonic/mod.rs L402-525  →0 CALLS 0.9 import_resolved @487
7 [1] in d1 Method InMemoryConnection.invoke  grpc/src/inmemory/mod.rs L305-335  →0 CALLS 0.9 import_resolved @312
8 [1] in d1 Method InMemoryTransport.connect  grpc/src/inmemory/mod.rs L434-471  →0 CALLS 0.9 import_resolved @455
9 [0] in d1 Method Sender.send  src/client/dispatch.rs L119-128  →0 CALLS 0.9 import_resolved @123
10 [0] in d1 Method Sender.try_send  src/client/dispatch.rs L107-116  →0 CALLS 0.9 import_resolved @111
(201 tests excluded; `include_tests: true` lists them)
(truncated; continue with "cursor": "10")
```

Median 57 ms. With `budget_tokens: 400` the answer stops at ten callers
and hands back a cursor. The header still lists tower (`[3]`), whose
callers were cut. Why it matters for an agent: a function with hundreds
of callers does not flood the context; the agent pays for what it reads
and pages for more.

## Natural-language questions (lexical ranking, today)

`search_symbols` ranks with BM25 over symbol names, signatures and doc
comments (Markdown sections included), weighted by repository routing. A
question works when its words match the names of the code that answers it,
and fails when the answer is named with other words.

### "where does axum turn a handler into a service"

```sh
rueca query search_symbols '{"query":"where does axum turn a handler into a service","limit":6}' --org tokio-rs --text
```

```
github.com/tokio-rs/axum@c59208c86fde (HEAD)
axum/src/handler/service.rs
  Method HandlerService.into_make_service L63-65  pub fn into_make_service(self) -> IntoMakeService<HandlerService<H, T, S>>
axum/src/handler/mod.rs
  Method HandlerWithoutStateExt.into_service L359  fn into_service(self) -> HandlerService<Self, T, ()>
  Interface HandlerWithoutStateExt L357-378  pub trait HandlerWithoutStateExt<T>: Handler<T, ()>
  Method HandlerWithoutStateExt.into_make_service L366  fn into_make_service(self) -> IntoMakeService<HandlerService<Self, T, ()>>
  Interface Handler L148-205  pub trait Handler<T, S>: Clone + Send + Sync + Sized + 'static
  Method H.into_make_service L388-390  fn into_make_service(self) -> IntoMakeService<HandlerService<Self, T, ()>>
(truncated; continue with "cursor": "6")
```

Good. The words "handler" and "service" are in the names, and every hit
is on the path from a handler to a service.

### "how does tokio steal work from other workers"

```sh
rueca query search_symbols '{"query":"how does tokio steal work from other workers","limit":6}' --org tokio-rs --text
```

```
github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
tokio/src/runtime/scheduler/multi_thread/worker.rs
  Method Core.steal_work L1168-1196  fn steal_work(&mut self, worker: &Worker) -> Option<Notified>
tokio/src/runtime/scheduler/multi_thread/queue.rs
  Method Steal.steal_into L417-468  pub(crate) fn steal_into( &self, dst: &mut Local<T>, dst_stats: &mut Stats, ) -> Option<task::Notified<T>>
  Struct Steal L34  pub(crate) struct Steal<T: 'static>(Arc<Inner<T>>)
tokio/src/runtime/scheduler/multi_thread/worker/taskdump.rs
  Method Shared.steal_all L59-75  pub(super) fn steal_all(&self) -> super::queue::Local<Arc<Handle>>
tokio/src/runtime/metrics/runtime.rs
  Method RuntimeMetrics.num_workers L48-50  pub fn num_workers(&self) -> usize
tokio/src/runtime/scheduler/mod.rs
  Method Handle.num_workers L214-220  pub(crate) fn num_workers(&self) -> usize
(truncated; continue with "cursor": "6")
```

Excellent. `Core.steal_work` and `Steal.steal_into` are the answer, first
and second.

### "how does tonic encode a message with prost"

```sh
rueca query search_symbols '{"query":"how does tonic encode a message with prost","limit":6}' --org tokio-rs --text
```

```
github.com/hyperium/tonic@4c0f455c264f (HEAD)
tonic-prost/README.md
  Section tonic-prost L1-19  # tonic-prost
  Section tonic-prost > Usage L9-19  ## Usage
tonic-prost-build/README.md
  Section tonic-prost-build L1-28  # tonic-prost-build
tonic-prost/src/codec.rs
  Method ProstEncoder.encode L127-132  fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error>
tonic-build/README.md
  Section tonic-build L1-3  # tonic-build
tonic-prost-build/src/lib.rs
  Struct TonicBuildService L152-155  struct TonicBuildService
(truncated; continue with "cursor": "6")
```

Good, with noise. `ProstEncoder.encode` is the answer, but three README
sections rank above it because they repeat "tonic" and "prost".

### "how does tokio decide which worker thread runs a task"

```sh
rueca query search_symbols '{"query":"how does tokio decide which worker thread runs a task","limit":6}' --org tokio-rs --text
```

```
github.com/tokio-rs/tokio@b53b2001ed2c (HEAD)
tokio/src/runtime/metrics/runtime.rs
  Method RuntimeMetrics.worker_thread_id L349-354  pub fn worker_thread_id(&self, worker: usize) -> Option<ThreadId>
tokio/src/runtime/scheduler/multi_thread/worker.rs
  Struct Worker L102-111  pub(super) struct Worker
  Method Context.run_task L648-797  fn run_task(&self, task: Notified, mut core: Box<Core>) -> ControlFlow<(), Box<Core>>
tokio/src/runtime/blocking/sharded.rs
  Method ShardedImpl.run_worker L238-331  pub(super) fn run_worker( &self, metrics: &SpawnerMetrics, keep_alive: Duration, worker_thread_id: usize, ) -> Option<thread::JoinHandle<()>>
tokio/src/runtime/blocking/pool.rs
  Method Spawner.spawn_task L462-506  fn spawn_task(&self, task: Task, rt: &Handle) -> Result<(), SpawnError>
  Method ThreadManagementState.worker_timed_out L172-178  pub(super) fn worker_timed_out( &mut self, worker_thread_id: usize, ) -> Option<thread::JoinHandle<()>>
(truncated; continue with "cursor": "6")
```

Mixed. `Worker` and `Context.run_task` are relevant; a metrics getter
(`RuntimeMetrics.worker_thread_id`, first) and the blocking pool hits are
noise. The decision itself lives in `Handle.schedule_task` and
`Core.next_task`, which share no word with the question.

### "how does hyper read the request body"

```sh
rueca query search_symbols '{"query":"how does hyper read the request body","limit":6}' --org tokio-rs --text
```

```
github.com/hyperium/hyper@7f67699ce32d (HEAD)
src/ffi/http_types.rs
  Function hyper_request_set_body L248-253  fn hyper_request_set_body(req: *mut hyper_request, body: *mut hyper_body) -> hyper_code
  Struct hyper_request L31  pub struct hyper_request(pub(super) Request<IncomingBody>)
src/ffi/body.rs
  Function hyper_body_free L78-80  fn hyper_body_free(body: *mut hyper_body)
  Struct hyper_body L33  pub struct hyper_body(pub(super) IncomingBody)
  Function hyper_body_set_data_func L195-198  fn hyper_body_set_data_func(body: *mut hyper_body, func: hyper_body_data_callback)
docs/ROADMAP-1.0.md
  Section hyper 1.0 Roadmap > Known Issues > You call hyper, or hyper calls you? L117-140  ### You call hyper, or hyper calls you?
(truncated; continue with "cursor": "6")
```

Bad. The FFI functions and types and a roadmap section match "hyper",
"request" and "body" literally. The answer lives in `Incoming.poll_frame`,
`Conn.poll_read_body` and `Decoder.decode`, named with other words.

## Natural-language questions with semantic search

Not run. Nothing in this section is output.

[RFC 0007](../rfcs/0007-semantic-search.md) (draft) adds embeddings as a
stage inside `search_symbols` and the organization ranking. When it lands,
I will re-run these questions on this exact organization, with the same
pins, and add the outputs here next to the lexical ones:

1. the five questions above;
2. "where does a connection get closed when the client goes away in hyper";
3. "what runs when a tonic server receives a request before the handler";
4. "how does tower's buffer layer apply backpressure".

## What does not work yet on this organization

The numbers come from `scripts/demo-tokio-rs.sh checks`
(`out/checks.txt`) unless a line says it was checked by hand.

### Fixed in `4905a37`

The first version of this page, built from `d569441`, listed four
failures. Three were the open findings of
[RFC 0006 §9.2](../rfcs/0006-org-graph.md#92-phase-2--implemented) (open
question 10 in §12); [PR #68](https://github.com/DevArKaDiA/rueca/pull/68)
fixed them and the fourth, with one part left open:

| Failure on `d569441` | Now | Before → after |
|---|---|---|
| Attribute macros produce no edge | fixed | 0 → 845 `DECORATES` link edges from tokio-macros |
| Items inside `cfg_*! { … }` bodies are not extracted | fixed, except re-export paths | 0 → 5 of 5 sampled items found; callers of `tokio::spawn` 0 → 92, of `TcpListener.accept` 0 → 34 |
| Method calls resolve to a same-named type of another crate | fixed | tokio → mio 200 → 158 edges, `type_resolved` CALLS 70 → 24; `Bytes::from` → hyper 29 → 0 |
| A function re-exported from a module of the same name has no callers | fixed | callers of `time::sleep`, `timeout`, `interval` 0, 0, 0 → 32, 12, 4 |

The details, measured again:

- **Attribute macros.** axum, hyper, tonic, tower and tracing contain 146
  lines that start with `#[tokio::main]` and 701 that start with
  `#[tokio::test]` (163 and 704 with doc comments). There are 146 and 699
  `DECORATES` link edges from tokio-macros (845; 847 with tracing's
  `#[instrument]`). Checked by hand: the 2 `#[tokio::test]` lines without
  an edge sit inside `macro_rules!` bodies (axum-extra's `cookie_test!`,
  tonic's `parametrized_tests!`), so they are templates, not items.

  | Repository | `#[tokio::main]` lines | edges | `#[tokio::test]` lines | edges |
  |---|---:|---:|---:|---:|
  | axum | 56 | 56 | 85 | 84 |
  | hyper | 15 | 15 | 106 | 106 |
  | tonic | 64 | 64 | 446 | 445 |
  | tower | 1 | 1 | 63 | 63 |
  | tracing | 10 | 10 | 1 | 1 |

- **Items inside `cfg_*!` bodies.** In `tokio/src` there are 340
  `cfg_*! {` occurrences, 287 outermost bodies, holding 522 of 3,317 `pub`
  item lines (15.7%). `get_symbol` with a name and a path now finds all
  five items I sampled: `TcpListener`, `UdpSocket`, `spawn`,
  `AsyncReadExt` and `AsyncReadExt.read` (the last one as `name: "read"`).
  Callers outside tests: `tokio::spawn` 92 (514 with tests),
  `TcpListener.accept` 34 (77 with tests), in axum, hyper, tokio, tonic
  and tracing. The part still open is the first item of the next list.
- **Same-named types of another crate.** The tokio → mio edges went from
  200 to 158: CALLS 52 (28 `import_resolved`, 24 `type_resolved`; was 97
  with 46 wrong), REFERENCES 77, IMPORTS 16, OVERRIDES 9, IMPLEMENTS 3,
  TESTS 1 (was 10, 9 of them wrong). Checked by hand against the source:
  15 of the 52 CALLS (7 `import_resolved`, 8 `type_resolved`), 15 right.
  Edges whose source lies in a dependency of the consumer went from 19 (13
  wrong) to 13; checked by hand, all 13 are `impl LocalTrait for
  ForeignType`, which is right (`impl IntoResponse for Bytes` in axum,
  `impl Connected for TcpStream` in tonic). `Bytes::from(…)` calls no
  longer link to hyper's `impl From<ReasonPhrase> for Bytes` (29 → 0); 73
  link edges now target bytes' own `Bytes.from`.
- **Re-exports from a module of the same name.**
  `tokio::time::sleep::sleep`, `timeout::timeout` and
  `interval::interval` have 32, 12 and 4 callers outside tests (206, 91
  and 16 with them). For scale, the five consumers contain 86
  `time::sleep(`, 77 `time::timeout(` and 4 `time::interval(` call
  sites, tests included. The controls moved by one: `Instant.now` 36 → 35
  and `watch::channel` 18 → 17; PR #68 says callers in modules declared
  by `#[cfg(test)] mod x;` are now test code.

### Still failing

1. **A re-export path is not a name.** `trace` with `qualified_name`
   `tokio::spawn`, `tokio::net::TcpListener.accept` or
   `tokio::time::sleep` answers "no symbol found"; the file's module path
   works (`tokio::task::spawn::spawn`,
   `tokio::net::tcp::listener::TcpListener.accept`,
   `tokio::time::sleep::sleep`). An agent that starts from the documented
   path has to search first. `get_symbol` with `name:
   "AsyncReadExt.read"` fails the same way, while `name: "read"` plus the
   path works.

2. **Methods of an extension trait implemented for every `Service` bind
   to nothing.** tower writes `impl<T: Service> ServiceExt for T`.
   `ServiceExt.oneshot` has 0 callers outside tests and 1 with them (a
   tower test), while axum, hyper, tonic, tower and tracing contain 73
   `.oneshot(` call sites, tests included; axum's
   `Route` calls it on lines 51 and 57 of `axum/src/routing/route.rs`
   (checked by hand).

3. **Re-indexing with nothing changed is not a no-op.** It takes 5.1 to
   5.4 s; every repository is replaced and the generation goes up. Only
   the ref is skipped.

Smaller things seen while writing this page:

- Ref comparison is per file blob, not per symbol (query 11): a symbol is
  ` changed` whenever its file changed.
- On `trace`, `repo` only selects the root. A `repo` list still returns
  callers from other repositories, and `repo: hyper` with a bytes symbol
  as the root answers "no symbol found".
- `get_symbol` with `name: "TcpListener.accept"` fails, while
  `qualified_name` plus `repo`, or `name: "accept"` plus the path, works.
- hyper shows an empty package name in `list_repositories`
  (`packages , hyper`).
- Routing ranks hyper above bytes for "byte buffer".
- With `budget_tokens`, the header can list a repository none of whose
  nodes is printed (tower in query 15).
- Skipped and partially parsed files are counted but not named: tracing
  2 skipped, axum 1, tonic 8 skipped and 2 partial.
- A search scoped to `repo: [tonic, axum]` for "graceful shutdown" shows
  only axum in the top 8.
- hyper's HTTP/1 `Connection.graceful_shutdown` has 1 caller (an
  example) and 1 test; checked by hand, the `Pin::new(&mut conn)
  .graceful_shutdown()` call sites in hyper's tests and in `http1.rs` are
  not among them. The HTTP/2 one has 0 callers.
- The 19 dangling contracts are all false positives: 18 are an
  `.unwrap()` after `XClient::connect(…).await`, read as an RPC `X/Unwrap`;
  1 is a hand-written wrapper named `BlockingClient`, read as a stub.

The 19 edges whose consumer and provider looked like the same repository
in the first version of this page were edges written in the consumer
with their source in a dependency; `out/links-summary.txt` now counts
them under that dependency.

## Reproduce it

```sh
scripts/demo-tokio-rs.sh            # all: clone, index, queries, checks, summary
scripts/demo-tokio-rs.sh clone      # clone the 8 repositories and check out the pins (the only network use)
scripts/demo-tokio-rs.sh index      # write rueca-org.toml, index, record status, links, contracts
scripts/demo-tokio-rs.sh queries    # the 27 queries: out/NN-<slug>.txt and out/timings.tsv
scripts/demo-tokio-rs.sh checks     # measure the weak spots: out/checks.txt
scripts/demo-tokio-rs.sh clean      # remove the store and out/; `clean --all` removes the clones too
```

Environment:

- `RUECA_DEMO_CACHE`: working directory, default
  `${TMPDIR:-/tmp}/rueca-demo-cache`.
- `RUECA`: the binary, default `rueca` on `PATH`.
- `RUECA_HOME`: the store, default `$RUECA_DEMO_CACHE/store`, so the demo
  never touches your own store.

Requirements: git, jq, perl (for millisecond timings) and python3 (one
estimate in `checks`). Disk: 256 MB of clones and about 195 MB of store.

Outputs in `$RUECA_DEMO_CACHE/out/`: `NN-<slug>.txt` for each query (the
first line is the exact command, the rest its exact output, errors
included), `timings.tsv` (median of three runs), `version.txt`,
`index.json` and `index-wall-ms.txt` (first index), `index-again.txt` and
`index-again-wall-ms.txt` (second index), `status.txt`,
`store-size-first.txt` and `store-size.txt`, `links.txt`, `links.json`
and `links-summary.txt`, `contracts.txt`, `contracts.json`,
`contracts-rpc.txt`, `contracts-dangling.txt` and `contracts-summary.txt`,
and `checks.txt`.

The pins are commits, not just tags: `clone` checks every tag against its
recorded commit and fails with "the tag moved" if one does not match.
Given the same binary, the query outputs are byte-identical from run to
run; the timings, the wall times and the `AGE` column of `status.txt` are
not.
