#!/usr/bin/env python3
"""Capture golden MCP transcripts from a `rueca` binary.

Spawns `rueca mcp` over stdio against a copy of `tests/fixtures/golden-repo`,
sends scripted JSON-RPC lines and records every stdout line the server writes
in answer. Standard library only; run with `python3 -I`:

    python3 -I crates/rueca-mcp/tests/golden/capture.py --bin path/to/rueca [--out DIR] [--only NAME ...]

Each transcript is written to `<out>/<name>.json` (default: this directory) as

    {"name": ..., "server": "rueca <version>", "description": ...,
     "sessions": [                      # one server process (connection) each
       {"label": ...,
        "steps": [
          {"send": <the message as JSON, or the raw string for non-JSON lines>,
           "send_raw": "<the exact line written, without the newline>",
           "wait": <the id waited for | "line" | null>,
           "note": "...",                       # optional
           "recv": [<parsed stdout lines>],     # {"$unparsed": line} if not JSON
           "recv_raw": ["<exact stdout lines, key order as emitted>"],
           "eof": true,                         # only if stdout closed here
           "timed_out": true,                   # only if the awaited id never came
           "skipped": "..."}, ...],             # only if the server had exited
        "exit": {"code": <exit status after stdin is closed>, "trailing": [...], "trailing_raw": [...]}}]}

`wait` = an id: the step ends when the response with that id arrives (10 s
timeout; anything else read meanwhile, e.g. notifications, is recorded too).
`"line"`: the step ends at the first line read (10 s timeout). `null`: the
step expects no answer and collects output for QUIET seconds.

The fixture is copied to `<tmp>/golden-repo` (outside any git repository) so
the snapshot identity is `local/golden-repo@worktree` on every machine; a
fresh store is used per session and the server indexes the root itself
(auto-index; tool calls wait for it). Any occurrence of the temp directory in
the output would be replaced by `$TMP` (see README.md: none occurs).
"""

import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.normpath(os.path.join(HERE, "..", "fixtures", "golden-repo"))
TIMEOUT = 10.0
QUIET = 1.0


def meta(version="2026-07-28", client_info=True):
    m = {"io.modelcontextprotocol/protocolVersion": version}
    if client_info:
        m["io.modelcontextprotocol/clientInfo"] = {"name": "golden", "version": "0"}
    m["io.modelcontextprotocol/clientCapabilities"] = {}
    return m


def req(id_, method, params=None, omit_params=False):
    msg = {"jsonrpc": "2.0", "id": id_, "method": method}
    if not omit_params:
        msg["params"] = {} if params is None else params
    return msg


def notif(method, params=None):
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def init(id_, version):
    return req(id_, "initialize", {
        "protocolVersion": version,
        "capabilities": {},
        "clientInfo": {"name": "golden", "version": "0"},
    })


def call(id_, name, arguments, with_meta=None):
    params = {}
    if with_meta is not None:
        params["_meta"] = with_meta
    params["name"] = name
    params["arguments"] = arguments
    return req(id_, "tools/call", params)


def step(send, wait="auto", note=None):
    """`wait="auto"`: the request's id, or no answer for a notification."""
    if wait == "auto":
        wait = send["id"] if isinstance(send, dict) and "id" in send else None
    s = {"send": send, "wait": wait}
    if note:
        s["note"] = note
    return s


# ---------------------------------------------------------------------------
# Scripts
# ---------------------------------------------------------------------------

TOOL_CALLS = [
    ("search_symbols", {"query": "total_due"}),
    ("search_symbols", {"query": "total_due", "format": "verbose"}),
    ("search_symbols", {"query": "a", "budget_tokens": 100}),
    ("search_symbols", {"query": "invoice", "limit": 1}),
    ("search_symbols", {"query": "invoice", "limit": 1, "cursor": "1"}),
    ("search_symbols", {"query": "invoice", "cursor": "zzz"}),
    ("get_symbol", {"qualified_name": "InvoiceService.issue"}),
    ("get_symbol", {"id": "local/golden-repo@worktree:web/client.ts#BillingClient.describe"}),
    ("get_snippet", {"qualified_name": "total_due"}),
    ("trace", {"qualified_name": "total_due"}),
    ("trace", {"qualified_name": "formatMoney", "direction": "callers"}),
    ("trace", {"qualified_name": "total_due", "direction": "in"}),
    ("search_code", {"pattern": "GOLDEN_MARKER_7F3A"}),
    ("get_file_outline", {"path": "billing/invoices.py"}),
    ("list_repositories", {}),
]


def legacy_main():
    steps = [
        step(init(1, "2025-06-18")),
        step(notif("notifications/initialized")),
        step(req(2, "ping")),
        step(req(3, "tools/list")),
    ]
    i = 10
    for name, args in TOOL_CALLS:
        steps.append(step(call(i, name, args)))
        i += 1
    steps += [
        step(call(30, "get_file_outline", {}), note="missing required argument: isError"),
        step(call(31, "trace", {"qualified_name": "total_due", "bogus": 1}), note="unknown argument: isError"),
        step(call(32, "nope", {}), note="unknown tool: JSON-RPC error"),
        step(req(33, "tools/call", {"name": "search_symbols"}), note="tools/call without `arguments`"),
        step(req(34, "foo/bar"), note="unknown method"),
        step(req(35, "tools/call", {}), note="invalid params shape for tools/call"),
        step(req(36, "tools/call", {"name": 5, "arguments": {}}), note="tools/call with a non-string name"),
        step(req(37, "tools/call", {"name": "search_symbols", "arguments": "x"}), note="arguments not an object"),
        step(notif("notifications/cancelled", {"requestId": 999, "reason": "golden"}),
             note="cancel of an id not in flight: expected no output"),
        step(req(38, "ping"), note="still alive after the cancellation"),
        step(req("abc", "ping"), note="string id"),
        step(req("abc", "tools/call", {"name": "list_repositories", "arguments": {}}), note="string id, tools/call"),
        step(req(None, "ping"), wait="line", note="id: null"),
        step(req(39, "ping"), note="still alive after id: null"),
        step(req(40, "tools/list", omit_params=True), note="tools/list with params omitted"),
        step(req(41, "tools/list", {"cursor": "zzz"}), note="tools/list with an unknown cursor"),
        step(req(42, "resources/list"), note="capability not advertised"),
        step(req(43, "prompts/list"), note="capability not advertised"),
        step(req(44, "logging/setLevel", {"level": "debug"}), note="capability not advertised"),
        step(req(45, "completion/complete", {"ref": {"type": "ref/prompt", "name": "x"},
                                            "argument": {"name": "a", "value": "b"}})),
        step(req(46, "server/discover", {"_meta": meta()}), note="server/discover after initialize"),
        step(init(47, "2025-06-18"), note="second initialize on the same connection"),
        step(req(48, "ping"), note="still alive after the second initialize"),
        step(req(49, "tools/list", {"_meta": {"progressToken": 7}}), note="legacy request with a progressToken"),
    ]
    return steps


def legacy_version(version):
    return lambda: [
        step(init(1, version)),
        step(notif("notifications/initialized")),
        step(req(2, "tools/list")),
        step(call(3, "search_symbols", {"query": "total_due"})),
    ]


def stateless():
    m = meta()
    return [
        step(req(1, "server/discover", {"_meta": m})),
        step(req(2, "tools/list", {"_meta": m})),
        step(call(3, "search_symbols", {"query": "total_due"}, with_meta=m)),
        step(call(4, "get_symbol", {"qualified_name": "InvoiceService.issue"}, with_meta=m)),
        step(call(5, "trace", {"qualified_name": "total_due"}, with_meta=m)),
        step(call(6, "trace", {"bogus": 1}, with_meta=m), note="unknown argument: isError"),
        step(call(7, "nope", {}, with_meta=m), note="unknown tool"),
        step(req(8, "ping", {"_meta": m}), note="ping with _meta"),
        step(req(9, "ping"), note="ping without _meta"),
        step(req(10, "tools/list"), note="tools/list without _meta"),
        step(call(11, "list_repositories", {}, with_meta=meta("2030-01-01")), note="unsupported version in _meta"),
        step(req(12, "tools/list", {"_meta": meta(client_info=False)}), note="_meta without clientInfo"),
        step(req(13, "tools/list", {"_meta": meta("2025-06-18")}), note="_meta naming a legacy version"),
        step(req(14, "foo/bar", {"_meta": m}), note="unknown method with _meta"),
        step(notif("notifications/initialized"), note="initialized notification without initialize"),
        step(req(15, "tools/list", {"_meta": m}), note="still alive?"),
    ]


def pre_init():
    return [
        ("requests before initialize, then initialize", [
            step(req(1, "ping"), note="ping before initialize"),
            step(req(2, "tools/list"), note="tools/list before initialize, no _meta"),
            step(call(3, "search_symbols", {"query": "total_due"}), note="tools/call before initialize, no _meta"),
            step(init(4, "2025-06-18"), note="initialize after stateless-mode errors"),
            step(req(5, "tools/list"), note="before notifications/initialized"),
            step(notif("notifications/initialized")),
            step(req(6, "tools/list")),
        ]),
        ("notifications/initialized as the first message", [
            step(notif("notifications/initialized"), note="the server exits (stderr: expect initialized request)"),
            step(req(1, "ping"), note="alive?"),
        ]),
        ("another notification as the first message", [
            step(notif("notifications/cancelled", {"requestId": 1}), note="first message is a notification"),
            step(req(1, "ping"), note="alive?"),
        ]),
        ("a response-shaped message as the first message", [
            step({"jsonrpc": "2.0", "id": 99, "result": {}}, wait=None),
            step(req(1, "ping"), note="alive?"),
        ]),
        ("a malformed line as the first message", [
            step("{not json", wait=None),
            step(req(1, "ping"), note="alive?"),
        ]),
        ("invalid requests as the first messages", [
            step([req(1, "ping"), req(2, "ping")], wait=None, note="JSON array batch"),
            step('"hi"', wait=None, note="a JSON string"),
            step(req(None, "ping"), wait="line", note="id: null"),
            step({"jsonrpc": "1.0", "id": 3, "method": "ping"}, wait=None, note="jsonrpc 1.0"),
            step(req(4, "ping"), note="alive?"),
        ]),
        ("stateless bootstrap errors before the lifecycle starts", [
            step(req(1, "server/discover"), note="discover without _meta"),
            step(req(2, "server/discover", {"_meta": meta("2030-01-01")}), note="discover naming an unsupported version"),
            step(req(3, "tools/list", {"_meta": meta("2030-01-01")}), note="unsupported version before the lifecycle"),
            step(req(4, "tools/list", {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
                 note="_meta without clientCapabilities"),
            step(req(5, "tools/list", {"_meta": meta("2025-06-18")}), note="full _meta naming a legacy version"),
            step(req(6, "tools/list"), note="without _meta, after the lifecycle started"),
            step(init(7, "2025-06-18"), note="initialize after a stateless lifecycle started"),
        ]),
    ]


def framing():
    ping = lambda i: step(req(i, "ping"))
    return [
        step(init(1, "2025-06-18")),
        step(notif("notifications/initialized")),
        step("{not json", wait=None, note="malformed JSON"),
        ping(2),
        step([req(3, "ping"), req(4, "ping")], wait=None, note="JSON array batch"),
        ping(5),
        step("", wait=None, note="empty line"),
        ping(6),
        step('"hi"', wait=None, note="a JSON string"),
        ping(7),
        step({"jsonrpc": "2.0", "id": 99, "result": {}}, wait=None, note="response-shaped message from the client"),
        ping(8),
        step({"jsonrpc": "1.0", "id": 9, "method": "ping"}, wait=None, note="jsonrpc 1.0"),
        ping(10),
        step({"id": 11, "method": "ping"}, wait=None, note="no jsonrpc member"),
        ping(12),
        step("   ", wait=None, note="whitespace-only line"),
        ping(13),
        step({"jsonrpc": "2.0", "id": 14, "method": "ping", "extra": True}, note="extra top-level member"),
        step({"jsonrpc": "2.0", "id": 1.5, "method": "ping"}, wait=None, note="fractional id"),
        ping(15),
        step('{"jsonrpc":"2.0","id":16,"method":"ping"}\r', note="CRLF line ending"),
    ]


def shutdown():
    return [
        ("after initialize", [
            step(init(1, "2025-06-18")),
            step(notif("notifications/initialized")),
            step(req(2, "ping")),
        ]),
        ("after server/discover (stateless)", [
            step(req(1, "server/discover", {"_meta": meta()})),
        ]),
        ("before any message", []),
        ("after a ping only (no initialize)", [
            step(req(1, "ping")),
        ]),
    ]


def unknown_versions():
    return [
        (f"initialize naming {v}", [step(init(1, v)), step(req(2, "ping"))])
        for v in ("1999-01-01", "2026-07-28", "2030-01-01")
    ]


def single(label, script):
    return lambda: [(label, script())]


TRANSCRIPTS = {
    "legacy-2025-06-18": ("Legacy initialize handshake at 2025-06-18; every tool and the error paths.",
                          single("legacy 2025-06-18", legacy_main)),
    "legacy-2025-11-25": ("Legacy initialize at 2025-11-25.", single("legacy 2025-11-25", legacy_version("2025-11-25"))),
    "legacy-2025-03-26": ("Legacy initialize at 2025-03-26.", single("legacy 2025-03-26", legacy_version("2025-03-26"))),
    "legacy-2024-11-05": ("Legacy initialize at 2024-11-05.", single("legacy 2024-11-05", legacy_version("2024-11-05"))),
    "initialize-unknown-version": ("initialize naming versions without a handshake, one connection each.",
                                   unknown_versions),
    "stateless-2026-07-28": ("Stateless 2026-07-28 flow: server/discover and per-request _meta, no initialize.",
                             single("stateless", stateless)),
    "pre-init": ("Messages before initialize, one connection per case.", pre_init),
    "framing": ("Malformed and unusual lines; a ping after each checks the server is still alive.",
                single("framing", framing)),
    "shutdown": ("stdin is closed at several points: exit status and trailing output.", shutdown),
}


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------

class Server:
    def __init__(self, binary, workdir):
        self.store = os.path.join(workdir, "store")
        os.makedirs(self.store)
        root = os.path.join(workdir, "golden-repo")
        shutil.copytree(FIXTURE, root)
        self.stderr = open(os.path.join(workdir, "stderr.log"), "wb")
        self.proc = subprocess.Popen(
            [binary, "mcp", "--store", self.store, "--root", root],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, cwd=workdir,
            env={**os.environ, "RUST_LOG": "info", "NO_COLOR": "1"},
        )
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for raw in iter(self.proc.stdout.readline, b""):
            self.lines.put(raw.decode("utf-8").rstrip("\n"))
        self.lines.put(None)

    def write(self, line):
        try:
            self.proc.stdin.write(line.encode("utf-8") + b"\n")
            self.proc.stdin.flush()
            return True
        except (BrokenPipeError, OSError):
            return False

    def collect(self, wait):
        """Lines until the answer to `wait` (see module doc)."""
        got = []
        eof = False
        deadline = time.monotonic() + (QUIET if wait is None else TIMEOUT)
        while True:
            left = deadline - time.monotonic()
            if left <= 0:
                break
            try:
                line = self.lines.get(timeout=left)
            except queue.Empty:
                break
            if line is None:
                eof = True
                break
            got.append(line)
            if wait == "line":
                break
            if wait is not None:
                try:
                    v = json.loads(line)
                except ValueError:
                    continue
                if isinstance(v, dict) and "id" in v and v["id"] == wait and type(v["id"]) is type(wait):
                    break
        return got, eof

    def close(self):
        """Closes stdin; returns (exit code or None, trailing lines)."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            code = self.proc.wait(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            code = None
        trailing, _ = self.collect(None)
        if code is None:
            self.proc.kill()
            self.proc.wait()
        self.stderr.close()
        return code, trailing


def parse(line):
    try:
        return json.loads(line)
    except ValueError:
        return {"$unparsed": line}


def run_session(binary, steps, tmp_names):
    workdir = tempfile.mkdtemp(prefix="rueca-golden-")
    tmp_names.append(workdir)
    server = Server(binary, workdir)
    out = []
    dead = False
    for s in steps:
        send = s["send"]
        raw = send if isinstance(send, str) else json.dumps(send, separators=(",", ":"), ensure_ascii=False)
        rec = {"send": send, "send_raw": raw, "wait": s["wait"]}
        if "note" in s:
            rec["note"] = s["note"]
        if dead or not server.write(raw):
            rec["recv"], rec["recv_raw"] = [], []
            rec["skipped"] = "server already exited"
            dead = True
            out.append(rec)
            continue
        lines, eof = server.collect(s["wait"])
        rec["recv"] = [parse(line) for line in lines]
        rec["recv_raw"] = lines
        awaited = s["wait"] not in (None, "line")
        if awaited and not eof and not any(isinstance(v, dict) and v.get("id") == s["wait"] for v in rec["recv"]):
            rec["timed_out"] = True
        if eof:
            rec["eof"] = True
            dead = True
        out.append(rec)
    code, trailing = server.close()
    with open(os.path.join(workdir, "stderr.log"), encoding="utf-8", errors="replace") as f:
        stderr = f.read()
    shutil.rmtree(workdir, ignore_errors=True)
    return out, {"code": code, "trailing": [parse(t) for t in trailing], "trailing_raw": trailing}, stderr


def normalise(obj, tmp_names):
    text = json.dumps(obj, ensure_ascii=False, indent=2)
    for t in tmp_names:
        for variant in {t, os.path.realpath(t)}:
            text = text.replace(variant, "$TMP")
    return text + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", required=True, help="the rueca binary")
    ap.add_argument("--out", default=HERE, help="output directory (default: this directory)")
    ap.add_argument("--only", nargs="*", help="transcript names to capture (default: all)")
    ap.add_argument("--stderr", action="store_true", help="print the server's stderr of each session")
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    version = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    for name in args.only or list(TRANSCRIPTS):
        desc, script = TRANSCRIPTS[name]
        tmp_names = []
        sessions = []
        for label, steps in script():
            steps, exit_, err = run_session(binary, steps, tmp_names)
            sessions.append({"label": label, "steps": steps, "exit": exit_})
            if args.stderr:
                sys.stderr.write(f"--- {name}: {label}\n{err}")
        doc = {"name": name, "server": version, "description": desc, "sessions": sessions}
        path = os.path.join(args.out, name + ".json")
        with open(path, "w", encoding="utf-8") as f:
            f.write(normalise(doc, tmp_names))
        n = sum(len(s["steps"]) for s in sessions)
        print(f"{name}: {len(sessions)} session(s), {n} steps -> {path}")

if __name__ == "__main__":
    main()
