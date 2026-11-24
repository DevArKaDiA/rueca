"""Embedding plumbing (embed.py): the `rueca embed` subprocess protocol
(JSONL in, f32le + ids out) against a fake binary, the content-hash vector
cache, Matryoshka truncation."""

import json

import numpy as np
import pytest

from rueca_bench.embed import CachedEmbedder, EmbedError, FakeEmbedder, RuecaEmbedder, VectorStore, text_key, truncate_normalize


def test_truncate_normalize_keeps_prefix_and_unit_norm():
    m = np.array([[3.0, 4.0, 12.0], [0.0, 0.0, 0.0]], dtype=np.float32)
    out = truncate_normalize(m, 2)
    assert out.shape == (2, 2)
    np.testing.assert_allclose(out[0], [0.6, 0.8], rtol=1e-6)
    np.testing.assert_allclose(out[1], [0.0, 0.0])
    np.testing.assert_allclose(np.linalg.norm(truncate_normalize(m)[0]), 1.0, rtol=1e-6)


def test_store_appends_and_a_second_reader_sees_the_rows(tmp_path):
    a = VectorStore(tmp_path / "ns")
    mat = np.arange(12, dtype=np.float32).reshape(3, 4)
    a.put_many(["k1", "k2", "k3"], mat)
    a.put_many(["k2", "k4"], np.ones((2, 4), dtype=np.float32))  # k2 kept as first written
    b = VectorStore(tmp_path / "ns")
    got = b.get_many(["k1", "k2", "k4", "missing"])
    assert set(got) == {"k1", "k2", "k4"}
    np.testing.assert_allclose(got["k2"], mat[1])
    np.testing.assert_allclose(got["k4"], np.ones(4))
    assert (tmp_path / "ns" / "keys.txt").read_text().splitlines() == ["k1", "k2", "k3", "k4"]
    with pytest.raises(EmbedError):
        b.put_many(["k5"], np.ones((1, 5), dtype=np.float32))


def test_cache_hits_misses_and_sides(tmp_path):
    fake = FakeEmbedder(16)
    c = CachedEmbedder(fake, tmp_path, chunk=2)
    first = c.embed(["def parse(x)", "class Config", "def parse(x)"], query=False)
    assert (c.misses, c.hits) == (2, 0)
    assert fake.calls == [2]
    again = CachedEmbedder(fake, tmp_path).embed(["class Config", "def parse(x)"], query=False)
    np.testing.assert_allclose(again[1], first[0], rtol=1e-3)  # float16 storage
    assert fake.calls == [2]  # all hits: no new call
    CachedEmbedder(fake, tmp_path).embed(["class Config"], query=True)  # queries: another namespace
    assert fake.calls == [2, 1]
    assert (tmp_path / "fake-16" / "doc" / "keys.txt").read_text().splitlines() == [text_key("def parse(x)"), text_key("class Config")]


def test_rueca_embedder_protocol_end_to_end(fake_rueca, tmp_path, monkeypatch):
    log = tmp_path / "argv.jsonl"
    monkeypatch.setenv("FAKE_RUECA_LOG", str(log))
    e = RuecaEmbedder(str(fake_rueca), "qwen3-0.6b-q8", max_tokens=256, batch=8)
    assert e.info()["model_key"] == "fake-qwen3-0.6b-q8"
    docs = e.embed(["alpha", "beta", "alpha"], query=False)
    assert docs.shape == (3, 8) and docs.dtype == np.float32
    np.testing.assert_array_equal(docs[0], docs[2])
    q_default = e.embed(["alpha"], query=True)
    assert not np.allclose(q_default[0], docs[0])  # the binary prefixes queries
    e.instruction = "Find the function that must change"
    q_custom = e.embed(["alpha"], query=True)
    assert not np.allclose(q_custom, q_default)
    calls = [json.loads(x) for x in log.read_text().splitlines()]
    embed_call = calls[-1]
    assert embed_call[:3] == ["embed", "--preset", "qwen3-0.6b-q8"]
    # a query call: the query budget (512, whatever the documents' budget)
    for flag, value in (("--max-tokens", "512"), ("--batch", "8"), ("--format", "f32le"), ("--instruction", "Find the function that must change")):
        assert embed_call[embed_call.index(flag) + 1] == value
    doc_call = next(c for c in calls if c[0] == "embed" and "--input" in c and "--instruction" not in c)
    assert doc_call[doc_call.index("--max-tokens") + 1] == "256"
    # queries cut at 512 never share the namespace of queries cut at 256
    assert e.namespace(True).startswith("fake-qwen3-0.6b-q8/t256/query-") and e.namespace(True).endswith("-q512")
    assert e.namespace(False) == "fake-qwen3-0.6b-q8/t256/doc"
    full = RuecaEmbedder(str(fake_rueca), "qwen3-0.6b-q8")
    assert full.namespace(True) == "fake-qwen3-0.6b-q8/t512/query-default" and full.namespace(False) == "fake-qwen3-0.6b-q8/t512/doc"


def test_rueca_embedder_errors_are_loud(fake_rueca, monkeypatch):
    with pytest.raises(EmbedError, match="model failed to load"):
        RuecaEmbedder(str(fake_rueca), "broken").embed(["x"], query=False)
    monkeypatch.setenv("FAKE_RUECA_SHUFFLE", "1")
    with pytest.raises(EmbedError, match="ids out of order"):
        RuecaEmbedder(str(fake_rueca)).embed(["x", "y"], query=False)
    monkeypatch.delenv("FAKE_RUECA_SHUFFLE")
    monkeypatch.setenv("FAKE_RUECA_NO_INSTRUCTION", "1")
    e = RuecaEmbedder(str(fake_rueca), instruction="custom")
    assert not e.supports_instruction()
    with pytest.raises(EmbedError, match="unexpected argument"):
        e.embed(["x"], query=True)
