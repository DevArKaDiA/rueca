"""Spanning gold (spanning.py): the library side of an organization question,
from the labeler's uses of the gold files and the fix patch's changed lines."""

from rueca_bench.crossrepo import GROUPS
from rueca_bench.spanning import changed_identifiers, org_pins, spanning_libraries, spanning_pairs

PATCH = """diff --git a/src/Cache/ArrayStore.php b/src/Cache/ArrayStore.php
--- a/src/Cache/ArrayStore.php
+++ b/src/Cache/ArrayStore.php
@@ -10,7 +10,7 @@ class ArrayStore
     use InteractsWithTime;
-        return Carbon::now()->getTimestamp();
+        return CarbonImmutable::now()->getTimestamp() + $this->seconds;
     // unchanged context: Logger
"""


def test_changed_identifiers_skip_headers_and_context():
    ids = changed_identifiers(PATCH)
    assert {"Carbon", "CarbonImmutable", "now", "getTimestamp", "seconds", "this"} <= ids
    assert "ArrayStore" not in ids and "Logger" not in ids and "InteractsWithTime" not in ids, "headers and context lines are not changed lines"


def test_spanning_libraries_whole_identifier_on_gold_files():
    uses = {
        ("src/Cache/ArrayStore.php", "briannesbitt/carbon", "Carbon\\Carbon"),
        ("src/Cache/ArrayStore.php", "briannesbitt/carbon", "Carbon\\CarbonInterval"),  # used, not in the patch
        ("src/Cache/ArrayStore.php", "Seldaek/monolog", "Monolog\\Logger"),  # only on a context line
        ("src/Other.php", "briannesbitt/carbon", "Carbon\\CarbonImmutable"),  # not a gold file
        ("src/Cache/ArrayStore.php", "briannesbitt/carbon", "Carbon\\Carb"),  # a prefix of `Carbon`: no substring match
    }
    paths = {"Carbon\\Carbon": "src/Carbon/Carbon.php"}
    got = spanning_libraries(uses, ["src/Cache/ArrayStore.php"], PATCH, "php", lambda lib, qn: paths.get(qn))
    assert got == [{"repo": "briannesbitt/carbon", "symbol": "Carbon\\Carbon", "path": "src/Carbon/Carbon.php", "consumer_files": ["src/Cache/ArrayStore.php"]}]


def test_rust_symbol_names_match_the_last_path_segment():
    patch = "@@ -1 +1 @@\n-    let l = tokio::net::TcpListener::bind(a);\n+    let l = bind_reuse(a);\n"
    uses = {("src/serve.rs", "tokio-rs/tokio", "tokio::net::tcp::listener::TcpListener"), ("src/serve.rs", "tokio-rs/tokio", "tokio::time.sleep")}
    got = spanning_libraries(uses, ["src/serve.rs"], patch, "rust", lambda lib, qn: "tokio/src/net/tcp/listener.rs")
    assert [x["symbol"] for x in got] == ["tokio::net::tcp::listener::TcpListener"]


def test_pairs_are_groups_edges_between_members_of_the_split():
    pins = org_pins("gated")
    pairs = spanning_pairs("gated")
    assert pairs, "the gated organization has consumer → library pairs"
    for consumer, pair in pairs.items():
        assert consumer in pins and all(lib["repo"] in pins for lib in pair["libraries"])
        assert any(g["consumer"] == consumer and {lib["repo"] for lib in pair["libraries"]} <= {x["repo"] for x in g["libraries"]} for g in GROUPS)
    assert spanning_pairs("calibration") == {}, "no calibration repository depends on another one"
