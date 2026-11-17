"""Service suite (service.py): Prometheus histogram quantiles, the
differential's first difference and the derived get_symbol / trace calls."""

from rueca_bench.service import (
    derived_calls,
    first_diff,
    histogram_quantile,
    quantile,
    server_stats,
)

METRICS = """# HELP rueca_mcp_request_seconds MCP tool call latency by tool and scope (repo, repos, org)
# TYPE rueca_mcp_request_seconds histogram
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="org",le="0.001"} 0
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="org",le="0.01"} 8
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="org",le="0.1"} 10
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="org",le="+Inf"} 10
rueca_mcp_request_seconds_sum{tool="search_symbols",scope="org"} 0.1
rueca_mcp_request_seconds_count{tool="search_symbols",scope="org"} 10
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="repo",le="0.001"} 10
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="repo",le="0.01"} 10
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="repo",le="0.1"} 10
rueca_mcp_request_seconds_bucket{tool="search_symbols",scope="repo",le="+Inf"} 10
rueca_mcp_request_seconds_sum{tool="search_symbols",scope="repo"} 0.005
rueca_mcp_request_seconds_count{tool="search_symbols",scope="repo"} 10
rueca_mcp_requests_total{tool="search_symbols",outcome="ok"} 20
rueca_generation 3
"""


def test_histogram_quantile_interpolates_like_prometheus():
    b = {0.001: 0.0, 0.01: 8.0, 0.1: 10.0, float("inf"): 10.0}
    assert abs(histogram_quantile(0.5, b) - (0.001 + 0.009 * 5 / 8)) < 1e-12
    assert abs(histogram_quantile(0.95, b) - (0.01 + 0.09 * 1.5 / 2)) < 1e-12
    # rank in +Inf: the last finite bound
    assert histogram_quantile(0.99, {0.1: 1.0, float("inf"): 10.0}) == 0.1
    assert histogram_quantile(0.5, {float("inf"): 0.0}) is None


def test_server_stats_sums_scopes_per_tool():
    s = server_stats(METRICS)
    t = s["tools"]["search_symbols"]
    assert t["n"] == 20 and t["mean_ms"] == 5.25
    # 20 calls: 10 under 1 ms (repo), 8 more under 10 ms, 2 under 100 ms
    assert t["p50_ms"] == 1.0
    assert s["by_scope"]["search_symbols/repo"]["n"] == 10
    assert s["requests_total"] == {"search_symbols": {"ok": 20}}
    assert s["generation"] == 3.0


def test_first_diff_names_the_path():
    assert first_diff({"a": [1, {"b": 2}]}, {"a": [1, {"b": 2}]}) is None
    assert first_diff({"a": [1, {"b": 2}]}, {"a": [1, {"b": 3}]})["path"] == "$.a[1].b"
    assert first_diff({"a": 1}, {"a": 1, "c": 0})["path"] == "$.c"
    assert first_diff([1], [1, 2])["path"] == "$.length"
    assert first_diff(1, 1.0)["path"] == "$"


def test_derived_calls_are_deterministic_and_trace_only_callables():
    answers = [
        {"results": [{"path": "a.py", "symbols": [{"id": "r@c:a.py#f", "label": "Function"}, {"id": "r@c:a.py#K", "label": "Class"}]}]},
        {"results": [{"path": "a.py", "symbols": [{"id": "r@c:a.py#f", "label": "Function"}, {"id": "r@c:a.py#K.m", "label": "Method"}]}]},
        None,
    ]
    calls = derived_calls(answers, 2)
    assert calls == derived_calls(answers, 2)
    assert [t for t, _ in calls].count("get_symbol") == 2
    traced = [a["id"] for t, a in calls if t == "trace"]
    assert traced == ["r@c:a.py#f", "r@c:a.py#K.m"]
    assert derived_calls(answers, 0) == []


def test_quantile_nearest_rank():
    xs = [float(i) for i in range(1, 21)]
    assert quantile(xs, 0.5) == 10.0
    assert quantile(xs, 0.95) == 19.0
    assert quantile([], 0.5) is None
