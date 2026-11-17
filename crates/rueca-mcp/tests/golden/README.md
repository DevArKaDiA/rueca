# Golden MCP transcripts

What Rueca's own MCP layer (`rueca 0.1.0-alpha.2`, the eight tools of
RFC 0006 phase 4 with `list_contracts`) writes on stdout for scripted
client input: the reference every change of the layer must reproduce byte
for byte (`recv_raw`). They were first recorded from the `rmcp`-based
server (`rmcp` 3.10.0, commit `c820290`) and replayed with the differences
of [`docs/mcp-protocol.md`](../../../../docs/mcp-protocol.md) §9 spelled
out in the test; the capture of the own layer made those differences part
of the files (the server now exits 0 at end of input in every session,
malformed `tools/call` params get -32602) and dropped the overrides.

The server is `rueca mcp --store <fresh tmp> --root <tmp>/golden-repo`, run on
a copy of [`../fixtures/golden-repo`](../fixtures/golden-repo) (Python,
TypeScript, Markdown; 55 lines), indexed by the server itself on start-up
(auto-index; tool calls wait for it). One server process per session.

| File | Sessions | Steps | What |
|---|---|---|---|
| `legacy-2025-06-18.json` | 1 | 43 | `initialize` 2025-06-18, `ping`, `tools/list`, every tool (compact and `verbose`, a cut response and its `cursor`, a bad cursor), argument errors (`isError`), unknown tool, unknown method, malformed `tools/call` params, `notifications/cancelled` for an id not in flight, string and `null` ids, `tools/list` without params and with cursor `zzz`, `resources/list`, `prompts/list`, `logging/setLevel`, `completion/complete`, `server/discover` after `initialize`, a second `initialize` |
| `legacy-2025-11-25.json`, `legacy-2025-03-26.json`, `legacy-2024-11-05.json` | 1 | 4 | `initialize` at that version, `tools/list`, one `tools/call` |
| `initialize-unknown-version.json` | 3 | 6 | `initialize` naming 1999-01-01, 2026-07-28 and 2030-01-01 (answered with 2025-11-25) |
| `stateless-2026-07-28.json` | 1 | 16 | no `initialize`: `server/discover` and per-request `_meta`; errors for missing `_meta`, unsupported version, `ping` in the stateless lifecycle |
| `pre-init.json` | 7 | 27 | messages before any lifecycle: requests without `_meta`, a notification or a response as the first message (ignored; the server stays up), invalid lines, stateless bootstrap errors |
| `framing.json` | 1 | 22 | malformed JSON, a batch array, empty and whitespace lines, a JSON string, a client response, `jsonrpc` 1.0, no `jsonrpc`, an extra member, a fractional id, CRLF; a `ping` after each |
| `shutdown.json` | 4 | 5 | stdin closed after `initialize`, after `server/discover`, before any message, after a `ping` only |

`../golden.rs` replays every session against the layer (in process, over
pipes) and compares every line byte for byte, with no override: a change
of the protocol, the instructions or the tool list is re-captured on
purpose (below) and reviewed in the diff.

## Format

```json
{"name": "...", "server": "rueca 0.1.0-alpha.2", "description": "...",
 "sessions": [{"label": "...",
   "steps": [{"send": {...}, "send_raw": "...", "wait": 3, "note": "...",
              "recv": [{...}], "recv_raw": ["..."]}],
   "exit": {"code": 0, "trailing": [], "trailing_raw": []}}]}
```

- `send_raw` is the exact line written (plus `\n`); `send` is the same
  message as JSON (or the raw string for lines that are not JSON).
- `recv_raw` are the exact stdout lines (key order as emitted), `recv` the
  same parsed. Everything read while a step waits is recorded, including
  notifications (none occur).
- `wait`: the id the step waits for (10 s timeout); `"line"`: the first line;
  `null`: no answer expected, output collected for 1 s.
- `eof`: stdout closed during the step; `skipped`: not sent because the server
  had exited; `timed_out`: the awaited id never came (none occur).
- `exit.code`: the exit status once stdin is closed (`trailing`: what it
  printed meanwhile).

## Re-capture

```sh
cargo build -p rueca
python3 -I crates/rueca-mcp/tests/golden/capture.py --bin target/debug/rueca
git diff --stat crates/rueca-mcp/tests/golden
```

`--out DIR` writes elsewhere (to compare a new binary without touching these
files), `--only NAME...` captures some files, `--stderr` prints the server's
logs. The debug and the release binary give the same files.

## Determinism and normalisation

Two captures in a row are identical, and no normalisation is applied:

- The fixture is copied outside any git repository, so the snapshot is
  `local/golden-repo@worktree (worktree)` with `overlay: false` on every
  machine (inside the Rueca checkout it would carry the checkout's origin and
  `HEAD` commit, which change with every commit).
- No response contains a path, a timestamp or a duration. The script would
  replace its temp directory by `$TMP` if one appeared; none does.
- The fixture's directory name is part of every id (`local/golden-repo`):
  keep it.
- The version (`serverInfo.version`, `_meta.io.modelcontextprotocol/serverInfo`)
  is `CARGO_PKG_VERSION`: a version bump changes those strings.
- Indexing honours the user's global gitignore; a global pattern matching
  `*.py`, `*.ts` or `*.md` would change the results.
