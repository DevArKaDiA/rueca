"""SQL: the labeler's callables (tables, views, routines, triggers, named
queries, dbt models and macros) and names, statement chunking (`GO`,
`DELIMITER`, `BEGIN … END`), and the code-backed gold (sqlgold.py):
dependency edges, CTEs, nearest-directory choice among copies, the trace
edge types of the Rueca arm."""

from pathlib import Path

from rueca_bench.common import is_code_file, lang_family, trace_edges
from rueca_bench.sqlgold import dialect_for, norm_name, sql_call_graph, sql_functions, statements, written_name
from rueca_bench.treesitter import extract_functions, grammar_for


def _names(src: str, dialect: str = "postgres", path: str = "db/schema.sql") -> list[tuple[str, int, int]]:
    return [(f.qualname, f.def_line, f.start) for f in sql_functions(path, src.encode(), dialect)]


def test_callables_and_names():
    src = 'CREATE TABLE public."Users" (id int);\nCREATE VIEW v AS SELECT id FROM "Users";\n\nCREATE TRIGGER t BEFORE UPDATE ON "Users" FOR EACH ROW EXECUTE FUNCTION f();\n'
    assert _names(src) == [("Users", 1, 1), ("v", 2, 2), ("Users.t", 4, 4)]
    assert _names("CREATE TABLE [dbo].[Orders] ([Id] INT)\nGO\n", "tsql") == [("orders", 1, 1)]
    assert _names('CREATE TABLE "ORDERS" (id int);', "snowflake") == [("orders", 1, 1)]
    assert norm_name([("main", False), ("t", False)], "sqlite") == "t"
    assert written_name("`proj`.`ds`.`T`", "bigquery") == "proj.ds.t"


def test_named_queries_not_pg_dump_headers():
    src = "-- name: GetUser :one\nSELECT * FROM users;\n\n--\n-- Name: actor; Type: TABLE; Schema: public\n--\n\nCREATE TABLE actor (id int);\n"
    assert [n for n, _, _ in _names(src)] == ["GetUser", "actor"]


def test_chunking():
    src = "DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END $$\nDELIMITER ;\nSELECT 3;\n"
    assert [s.text for s in statements(src, "mysql")] == ["CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END", "SELECT 3"]
    src = "CREATE TRIGGER t AFTER UPDATE ON c BEGIN UPDATE c SET x = 1; END;\nSELECT 1;"
    assert len(statements(src, "sqlite")) == 2
    src = "CREATE PROCEDURE p AS\nSELECT 1;\nSELECT 2;\nGO\nSELECT 3\n"
    assert [s.line for s in statements(src, "tsql")] == [1, 5]


def test_grammar_files_and_edges():
    for p in ("a.sql", "a.pgsql", "a.tsql", "a.bq", "a.mysql"):
        assert grammar_for(p) == "sql", p
        assert is_code_file(p, "sql")
    assert lang_family("sql") == "sql"
    assert trace_edges("sql") == ["CALLS", "REFERENCES", "WRITES"]
    assert trace_edges("python") is None
    assert [f.qualname for f in extract_functions("x.sql", b"CREATE VIEW v AS SELECT 1;")] == ["v"]
    assert dialect_for("a/b.sql", {"": "dbt", "a/": "mysql"}) == "mysql"


def _write(root: Path, files: dict[str, str]) -> None:
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)


def _edges(graph: dict) -> set[tuple[str, str]]:
    name = lambda k: k.split("#", 1)[1].rsplit("@", 1)[0]  # noqa: E731
    return {(name(a), name(b)) for a, bs in graph["callees"].items() for b in bs}


def test_gold_edges(tmp_path: Path):
    _write(
        tmp_path,
        {
            "pg/schema.sql": "CREATE TABLE a (id int);\nCREATE TABLE b (a_id int REFERENCES a (id));\n"
            "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN (SELECT count(*) FROM b); END $$ LANGUAGE plpgsql;\n"
            "CREATE VIEW v AS WITH a AS (SELECT 1 AS id) SELECT f(), b.a_id FROM b JOIN a ON TRUE;\n"
            "INSERT INTO a VALUES (1);\n",
            "my/schema.sql": "CREATE TABLE a (id int);\n",
            "other/report.sql": "CREATE VIEW r AS SELECT id FROM a;\n",
        },
    )
    g = sql_call_graph(tmp_path, [""], dialects={"pg/": "postgres", "my/": "mysql"})
    # The CTE `a` shadows table `a`; the insert is file level; `r` sees two
    # copies of `a` at the same distance: no edge.
    assert _edges(g) == {("b", "a"), ("f", "b"), ("v", "f"), ("v", "b")}


def test_dbt_models_and_macros(tmp_path: Path):
    _write(
        tmp_path,
        {
            "models/stg.sql": "select * from {{ source('s', 't') }}",
            "models/mart.sql": "{{ config(materialized='table') }}\nselect {{ money('x') }} from {{ ref('stg') }}",
            "macros/m.sql": "{% macro money(c) %}({{ c }} / 100){% endmacro %}\n{% macro tax(c) %}{{ money(c) }} * 1.2{% endmacro %}",
        },
    )
    g = sql_call_graph(tmp_path, [""], dialects={"": "dbt"})
    assert _edges(g) == {("mart", "stg"), ("mart", "money"), ("tax", "money")}


def test_oracle_and_db2_units():
    # Oracle: `/` ends a PL/SQL unit, a unit after other statements of its
    # buffer runs to the `/`; bodies are read as text; a quoted upper-case
    # name is the unquoted one.
    src = (
        'CREATE TABLE "PAYMENT" (id NUMBER(10), amount NUMBER(8,2));\n'
        "CREATE OR REPLACE FUNCTION total(p IN NUMBER) RETURN NUMBER IS\n  l NUMBER;\nBEGIN\n"
        "  SELECT SUM(amount) INTO l FROM payment WHERE id = p;\n  RETURN l;\nEND;\n/\n"
        "CREATE OR REPLACE TRIGGER payment_bu\nBEFORE UPDATE ON payment\nFOR EACH ROW\nBEGIN\n  :NEW.amount := 0;\nEND;\n/\n"
        "CREATE VIEW v AS SELECT id FROM payment WHERE total(id) > 1;\n"
    )
    assert [t.text.split("\n")[0][:28] for t in statements(src, "oracle")] == [
        'CREATE TABLE "PAYMENT" (id N',
        "CREATE OR REPLACE FUNCTION t",
        "CREATE OR REPLACE TRIGGER pa",
        "CREATE VIEW v AS SELECT id F",
    ]
    assert [n for n, _, _ in _names(src, "oracle")] == ["payment", "total", "payment.payment_bu", "v"]
    # the same text in another dialect is cut at every `;` outside BEGIN … END, as before
    assert len(statements(src, "postgres")) > 4
    # DB2: `--#SET TERMINATOR @`
    db2 = "--#SET TERMINATOR @\nCREATE TABLE a (id INT)@\nCREATE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DELETE FROM a;\nEND@\n"
    assert [n for n, _, _ in _names(db2, "db2")] == ["a", "p"]


def test_oracle_gold_edges(tmp_path: Path):
    (tmp_path / "schema.sql").write_text(
        "CREATE TABLE payment (id NUMBER(10), amount NUMBER(8,2));\n"
        "CREATE OR REPLACE FUNCTION total(p IN NUMBER) RETURN NUMBER IS\n  l NUMBER;\nBEGIN\n"
        "  SELECT SUM(amount) INTO l FROM payment WHERE id = p;\n  RETURN l;\nEND;\n/\n"
        "CREATE OR REPLACE TRIGGER payment_bu\nBEFORE UPDATE ON payment\nFOR EACH ROW\nBEGIN\n  :NEW.amount := 0;\nEND;\n/\n"
        "CREATE OR REPLACE PROCEDURE purge(p IN NUMBER) IS\nBEGIN\n  IF total(p) = 0 THEN\n    DELETE FROM payment WHERE id = p;\n  END IF;\nEND;\n/\n"
    )
    g = sql_call_graph(tmp_path, [""], dialects={"": "oracle"})
    name = {k: d["qualname"] for k, d in g["callables"].items()}
    edges = sorted((name[a], name[b]) for a, bs in g["callees"].items() for b in bs)
    assert edges == [("payment.payment_bu", "payment"), ("purge", "payment"), ("purge", "total"), ("total", "payment")]
