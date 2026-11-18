"""`rueca query --batch <jsonl>`: many tool calls answered by one process
(RFC 0007 phase 3), so a model or reranker loads once instead of once per
call. The bench codes against this contract:

- input: a JSON Lines file, one request per line, `{"tool": <name>,
  "args": {...}}` (the arguments exactly as `rueca query <tool> '<args>'`
  takes them);
- the process flags are those of a single call (`--root` or `--org`,
  `--store`; never `--text`);
- output: one line per request, in input order, each the JSON a single
  call prints (the tool's envelope, or `{"error": "..."}` for that request
  alone); the process exits 0 when it answered every line, and each line is
  flushed when written.

Latency per request is the time between consecutive output lines (the
first one also holds process start-up and the first model load: reported
apart as `first_s`, never in a warm percentile). Support is detected from
`rueca query --help` naming `--batch`; without it callers fall back to one
process per call.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class BatchLine:
    """One answered request: parsed JSON (None when unreadable), seconds
    since the previous line, error, raw line."""

    data: Any
    seconds: float
    error: str | None
    raw: str


@dataclass
class BatchRun:
    lines: list[BatchLine]
    rc: int
    seconds: float
    stderr: str

    @property
    def first_s(self) -> float | None:
        return self.lines[0].seconds if self.lines else None

    def warm_seconds(self) -> list[float]:
        """Per-request seconds after the first answer (model loaded)."""
        return [x.seconds for x in self.lines[1:] if x.error is None]


def parse_line(raw: str, seconds: float) -> BatchLine:
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return BatchLine(None, seconds, f"non-json batch line: {raw[:300]}", raw)
    if isinstance(data, dict) and "error" in data:
        return BatchLine(data, seconds, f"rc=2: {data.get('error')}", raw)
    return BatchLine(data, seconds, None, raw)


def write_requests(path: Path, requests: list[tuple[str, dict]]) -> None:
    with path.open("w", encoding="utf-8") as fh:
        for tool, args in requests:
            fh.write(json.dumps({"tool": tool, "args": args}, ensure_ascii=False) + "\n")


def _read(proc: subprocess.Popen, t0: float) -> list[BatchLine]:
    out: list[BatchLine] = []
    prev = t0
    assert proc.stdout is not None
    for raw in proc.stdout:
        now = time.perf_counter()
        if raw.strip():
            out.append(parse_line(raw.strip(), now - prev))
            prev = now
    return out


def run_batch(cmd: list[str], requests: list[tuple[str, dict]], env: dict[str, str] | None, timeout: float) -> BatchRun:
    """Run `cmd + ["--batch", <file>]` over `requests`; a request without an
    answer line (crash, timeout) gets an error line."""
    full_env = {**os.environ, **(env or {})}
    with tempfile.TemporaryDirectory(prefix="rueca-batch-") as tmp:
        src, err_path = Path(tmp) / "requests.jsonl", Path(tmp) / "stderr.txt"
        write_requests(src, requests)
        t0 = time.perf_counter()
        with err_path.open("w") as err:
            proc = subprocess.Popen([*cmd, "--batch", str(src)], stdout=subprocess.PIPE, stderr=err, text=True, errors="replace", env=full_env)
            timer = threading.Timer(timeout, proc.kill)
            timer.start()
            try:
                lines = _read(proc, t0)
                rc = proc.wait()
            finally:
                timer.cancel()
        stderr = err_path.read_text(errors="replace")[-1500:]
    total = time.perf_counter() - t0
    missing = len(requests) - len(lines)
    if missing > 0:
        why = f"batch answered {len(lines)} of {len(requests)} requests (rc={rc}): {stderr[-300:]}"
        lines += [BatchLine(None, 0.0, why, "") for _ in range(missing)]
    return BatchRun(lines[: len(requests)], rc, total, stderr)


def batch_timeout(per_call: float, n: int) -> float:
    """A whole batch may take as long as its calls one by one."""
    return per_call * max(1, n) + 300.0
