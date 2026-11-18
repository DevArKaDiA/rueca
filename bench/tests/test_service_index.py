"""Service indexing suite (service_index.py): webhook payloads and
signatures, and the per-stage quantiles of bucket deltas."""

import hashlib
import hmac
import json

from rueca_bench.service_index import github_push, proc_config, signature, stage_stats


def test_github_push_is_signed_like_github():
    body = github_push("git://127.0.0.1:9418/billing.git", "a" * 40)
    payload = json.loads(body)
    assert payload["ref"] == "refs/heads/main" and payload["after"] == "a" * 40
    assert payload["repository"]["clone_url"] == "git://127.0.0.1:9418/billing.git"
    want = hmac.new(b"s3cret", body, hashlib.sha256).hexdigest()
    assert signature("s3cret", body) == f"sha256={want}"


def test_stage_stats_use_the_deltas_only():
    before = {"fetch": {0.1: 2.0, 1.0: 2.0, float("inf"): 2.0}}
    after = {
        "fetch": {0.1: 2.0, 1.0: 6.0, float("inf"): 6.0},
        "generation": {0.1: 0.0, 1.0: 1.0, float("inf"): 1.0},
    }
    s = stage_stats(before, after)
    # four new fetches, all between 0.1 and 1 s
    assert s["fetch"]["n"] == 4
    assert 100 < s["fetch"]["p50_ms"] < 1000
    assert s["generation"]["n"] == 1
    assert "index" not in s


class _Host:
    sources = ()

    def url(self, name):
        return f"git://127.0.0.1:1/{name}.git"


class _Proc:
    name, role, updater, port, ops = "indexer-0", "indexer", True, 1, 2


def test_config_enables_the_test_host_and_webhooks(tmp_path):
    text = proc_config(_Proc(), tmp_path, "file:///tmp/store", _Host(), "rueca_x", 15)
    for needle in ("allow_file_urls = true", 'hosts = ["127.0.0.1"]', "org_updater = true", 'wall_time = "15s"', 'github = "env:RUECA_BENCH_GITHUB_SECRET"'):
        assert needle in text, needle
