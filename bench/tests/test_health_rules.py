"""Gated parse-error rate: which files are left out (health.py)."""

from rueca_bench.health import exclusion_reason


def test_vendored_like_the_ranking():
    for p in ["third_party/zlib/inflate.c", "src/deps/lua/lapi.c", "dependencies/jsoncpp/json.h", "vendor/x/y.c", "a/3rdparty/b.h"]:
        assert exclusion_reason(p, "c") == "vendored", p
    for p in ["deps.c", "src/dependencies.c", "src/vendoring/x.c", "lib/third_party_api.c"]:
        assert exclusion_reason(p, "c") is None, p


def test_gtest_and_amalgamations():
    assert exclusion_reason("test/gtest/gtest-all.cc", "cpp") == "gtest copy"
    assert exclusion_reason("googletest/include/gtest/gtest.h", "cpp") == "gtest copy"
    assert exclusion_reason("single_include/nlohmann/json.hpp", "cpp") == "amalgamation"
    assert exclusion_reason("singleheader/simdjson.h", "cpp") == "amalgamation"
    assert exclusion_reason("extras/catch_amalgamated.cpp", "cpp") == "amalgamation"
    assert exclusion_reason("include/nlohmann/json.hpp", "cpp") is None


def test_per_repository_trees():
    assert exclusion_reason("lib/littlefs/lfs2.c", "c", "micropython/micropython") == "vendored"
    assert exclusion_reason("lib/littlefs/lfs2.c", "c", "other/repo") is None
    assert exclusion_reason("ports/cc3200/FreeRTOS/Source/queue.c", "c", "micropython/micropython") == "vendored"
    assert exclusion_reason("ports/cc3200/mods/pybuart.c", "c", "micropython/micropython") is None
    assert exclusion_reason("tests/abi/include/nlohmann/json_v3_10_5.hpp", "cpp", "nlohmann/json") == "amalgamation"


def test_not_the_language(tmp_path):
    (tmp_path / "py").mkdir()
    (tmp_path / "py" / "qstrdefs.h").write_text('#include "py/mpconfig.h"\nQCFG(BYTES_IN_LEN, 1)\nQ()\nQ(*)\n')
    (tmp_path / "py" / "qstr.h").write_text("#define Q(x) x\nint f(void);\n")
    (tmp_path / "TestObj.h").write_text("#import <Foundation/Foundation.h>\n@interface TestObj : NSObject\n@end\n")
    (tmp_path / "plain.h").write_text("/* @interface in a comment */\nint g(void);\n")
    assert exclusion_reason("py/qstrdefs.h", "c", "micropython/micropython", tmp_path) == "not the language (qstr list)"
    assert exclusion_reason("py/qstr.h", "c", "micropython/micropython", tmp_path) is None
    assert exclusion_reason("TestObj.h", "cpp", "catchorg/Catch2", tmp_path) == "not the language (Objective-C)"
    assert exclusion_reason("plain.h", "c", "x/y", tmp_path) is None
    assert exclusion_reason("TestObj.h", "python", "x/y", tmp_path) is None
