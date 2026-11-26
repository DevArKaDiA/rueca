"""Construct categories behind the strict parse errors of the held-out SQL
corpus, by file (a file with several causes is in several rows):

    python3 bench/datasets/heldout/sql_causes.py bench/datasets/heldout/sql_heldout_files.json [-v]

Descriptive only: the categories were written after the measurement, from
the parser errors and the first words of the head-only statements recorded
by sql_by_dialect.py. No parser change followed.
"""
import json,collections,re,sys
d=json.load(open(sys.argv[1]))
def cat(repo,label,k):
    kind,_,rest=k.partition(" | ")
    K=rest.upper()
    if re.search(r"<\w+ ?,", rest) or "found: <" in rest or re.search(r"USE <|EXECUTE <|EXEC <", K): return "SSMS template parameters `<name, type, value>`"
    if "#-" in rest or re.search(r"found: #", rest): return "`#` line comments (BigQuery Jinja sources, MySQL)"
    if re.search(r"found: - \| --", rest): return "bare `--` line with no space after it (MySQL comment rule)"
    if "DELIMITER" in K or rest.endswith("| d//") or "found: d | d//" in rest: return "MySQL `delimiter` with a word delimiter (`d//`)"
    if kind=="head-only" and re.search(r"^(CREATE|DROP|ALTER) EVENT", K) and label=="MySQL / MariaDB": return "MySQL scheduled events (`CREATE / DROP / ALTER EVENT`)"
    if "AS, found: (" in rest and "age" in repo: return "PostgreSQL column definition list `AS (v agtype)` after a set-returning function (Apache AGE `cypher()`)"
    if label=="Snowflake" or repo.endswith("sf-samples"):
        if kind=="head-only" and re.search(r"^USE ", K): return "Snowflake `USE ROLE / DATABASE / SCHEMA / WAREHOUSE` (head only)"
        if kind=="head-only": return "Snowflake account and object statements read by head only (`GRANT`, `CREATE DATABASE / SCHEMA / ROLE / STAGE / STREAM / TASK`, `ALTER WAREHOUSE`, …)"
        return "Snowflake statements with no grammar (dynamic tables, compute pools, image repositories, Streamlit, `$` variables, scripting)"
    if label=="Oracle":
        if re.search(r"SET DEFINE|found: rem|found: REM|found: Rem|PROMPT|SPOOL|^SET |found: var", rest, ) or (kind=="head-only" and re.search(r"^(SET|PROMPT|SPOOL|WHENEVER|DEFINE|COLUMN|ACCEPT|CONNECT|EXECUTE|EXEC|VAR|VARIABLE) ", K+" ")): return "Oracle SQL*Plus commands (`SET DEFINE`, `rem`, `PROMPT`, …)"
        if "found: ," in rest and "INSERT" in K: return "Oracle SQL*Plus line continuation `-` inside a statement"
        return "Oracle DDL / PL/SQL forms with no grammar (object types, `CREATE USER`, storage clauses, …)"
    if label=="T-SQL":
        if "�" in rest or re.search(r"S E L E C T|G O ", rest): return "UTF-16 encoded files"
        if kind=="head-only": return "T-SQL statements read by head only (`ALTER DATABASE`, `CREATE STATISTICS`, `CREATE EXTERNAL …`, `RESTORE`, partition functions, event sessions, `ALTER / DROP TABLE` variants, …)"
        return "T-SQL statements no dialect parses (natively compiled `BEGIN ATOMIC`, `OPENJSON … WITH`, graph `MATCH`, `IF` without `BEGIN`, …)"
    if label=="MySQL / MariaDB":
        if kind=="head-only": return "MySQL statements read by head only (`ALTER TABLE` variants, `CREATE USER`, `GRANT`, …)"
        return "MySQL statements no dialect parses (column `charset`, multi-table `UPDATE`, …)"
    if label=="PostgreSQL":
        if kind=="head-only": return "PostgreSQL statements read by head only (`CREATE CAST`, `CREATE OPERATOR`, `CREATE EVENT TRIGGER`, `ALTER ROLE`, `DO LANGUAGE`, …)"
        return "PostgreSQL statements no dialect parses (operators, psql variables, …)"
    if kind=="head-only": return f"{label}: other statements read by head only"
    return f"{label}: other statements no dialect parses"
files=collections.defaultdict(set); ex=collections.defaultdict(collections.Counter)
for r in d["repos"]:
    for f,v in r["failing"].items():
        for k in v["causes"]:
            c=cat(r["repo"],v["label"],k); files[c].add((r["repo"],f)); ex[c][(r["repo"].split("/")[1], k[:90])]+=1
tot=sum(len(r["failing"]) for r in d["repos"])
for c,fs in sorted(files.items(), key=lambda kv:-len(kv[1])):
    reps=collections.Counter(r.split("/")[1] for r,_ in fs)
    print(len(fs), "|", c, "|", ", ".join(f"{k} {v}" for k,v in reps.most_common(4)))
    if "-v" in sys.argv[2:]:
        for (rp,k),n in ex[c].most_common(8): print("      ",n,rp,k)
print(tot)
