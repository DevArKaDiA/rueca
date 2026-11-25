#!/usr/bin/env python3
"""Mine issue -> fix localization pairs (S1 format) for shell and SQL.

Deterministic and re-runnable: every GitHub API response is cached under
`--cache` (key = sha1 of the request), so a second run is offline. Only
PRs merged before CUTOFF are considered.

Usage:
  python3 -I mine_s1.py --lang shell --repos owner/a owner/b ... \
      --cache ../data/api --out ../out/rueca_mined_shell.jsonl --stats ../out/stats_shell.json

Screening rules (counts dropped per rule are written to --stats):
  no_issue      PR closes no issue of the same repository (closingIssuesReferences)
  bot           every closed issue was opened by a bot
  issue_after   an issue was opened at/after the PR
  short         title + body prose < 100 characters (HTML comments, fenced
                code, template headings and check-boxes stripped)
  leak          issue text carries a diff / patch block
  no_lang       the PR modifies no existing non-test file of the language
  not_mostly    language files are not > 50% of the changed non-doc, non-test files
  too_many      more than 10 language files changed
  truncated     more than 100 changed files (GraphQL file list incomplete)
  no_diff       diff / compare not available from the API (merge base unknown)
  too_big       patch > 100 KB after dropping binary files
  no_lang_patch the unified diff (after the checks above) has no existing language file
  unreachable   base commit not served by the API
  dup_issue     a PR merged earlier already closes the same issue
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import time
from pathlib import Path

CUTOFF = "2026-11-25T00:00:00Z"

EXT = {
    "shell": (".sh", ".bash", ".zsh", ".ksh", ".bats"),
    "sql": (".sql", ".psql", ".pgsql", ".plpgsql", ".mysql", ".tsql", ".bq"),
}
# bench/src/rueca_bench/common.py: is_test_path (built-in segments + the
# manifest's per-language test_dirs / test_files that can match these files)
TEST_SEGMENTS = {"test", "tests", "__tests__", "__mocks__", "__fixtures__", "e2e", "spec", "specs", "testdata", "Tests"}
TOP_TEST = {"testing"}
# stricter than the harness: dbt packages keep their test project here
EXTRA_TEST_SEGMENTS = {"integration_tests", "integration-tests", "test_helper", "fixtures", "t"}
TEST_FILE = re.compile(
    r"^(?:.*\.bats|test_.*\.(?:sh|bash|zsh|ksh)|.*_test\.(?:sh|bash|zsh|ksh)|.*_spec\.sh|test_.*\.sql|.*_test\.sql)$"
)
DOC_EXT = (".md", ".markdown", ".rst", ".txt", ".adoc", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".pdf", ".html", ".css")
DOC_NAMES = {"license", "licence", "notice", "authors", "changelog", "changes", "news", "readme", "contributing", "codeowners", ".gitignore", ".gitattributes", ".editorconfig", "version"}
DOC_DIRS = {"docs", "doc", ".github", ".circleci", ".buildkite", "man", ".changes"}
# neutral for SQL: dbt property / project files travel with every model change
SQL_NEUTRAL_EXT = (".yml", ".yaml", ".csv")


def is_test(path: str) -> bool:
    parts = path.split("/")
    if parts[0] in TOP_TEST:
        return True
    if any(p in TEST_SEGMENTS or p in EXTRA_TEST_SEGMENTS for p in parts[:-1]):
        return True
    return bool(TEST_FILE.match(parts[-1]))


def is_doc(path: str, lang: str) -> bool:
    parts = path.split("/")
    name = parts[-1].lower()
    if name.endswith(DOC_EXT) or name.split(".")[0] in DOC_NAMES:
        return True
    if any(p.lower() in DOC_DIRS for p in parts[:-1]):
        return True
    if name in (".travis.yml", "appveyor.yml", ".pre-commit-config.yaml", "renovate.json", "dependabot.yml"):
        return True
    return lang == "sql" and name.endswith(SQL_NEUTRAL_EXT)


def is_lang(path: str, lang: str) -> bool:
    return path.endswith(EXT[lang])


class Api:
    def __init__(self, cache: Path):
        self.cache = cache
        cache.mkdir(parents=True, exist_ok=True)
        self.calls = 0

    def _run(self, args: list[str], stdin: str | None = None) -> tuple[int, str, str]:
        for attempt in range(6):
            p = subprocess.run(["gh", "api", *args], input=stdin, capture_output=True, text=True)
            self.calls += 1
            err = p.stderr.lower()
            if p.returncode == 0:
                return 0, p.stdout, p.stderr
            if "api rate limit exceeded" in err:
                q = subprocess.run(["gh", "api", "rate_limit", "--jq", "[.resources.core.reset,.resources.graphql.reset,.resources.core.remaining,.resources.graphql.remaining]"], capture_output=True, text=True)
                try:
                    cr, gr, crem, grem = json.loads(q.stdout)
                    reset = cr if crem == 0 else gr if grem == 0 else int(time.time()) + 60
                except Exception:  # noqa: BLE001
                    reset = int(time.time()) + 120
                time.sleep(max(5, min(3700, reset - int(time.time()) + 5)))
                continue
            if "rate limit" in err or "secondary" in err or "502" in err or "503" in err or "504" in err or "timeout" in err or "eof" in err:
                time.sleep(15 * (attempt + 1))
                continue
            return p.returncode, p.stdout, p.stderr
        return 1, "", "retries exhausted"

    def get(self, key_parts: list[str], args: list[str], ok_fail: bool = False) -> str | None:
        key = hashlib.sha256("\x00".join(key_parts).encode()).hexdigest()
        f = self.cache / key[:2] / key
        fail = f.with_suffix(".fail")
        if f.exists():
            return f.read_text(encoding="utf-8", errors="replace")
        if fail.exists():
            return None
        rc, out, err = self._run(args)
        f.parent.mkdir(parents=True, exist_ok=True)
        if rc != 0:
            if ok_fail:
                fail.write_text(err)
                return None
            raise RuntimeError(f"gh api {' '.join(args)[:200]}: {err[:300]}")
        f.write_text(out, encoding="utf-8")
        return out


PR_QUERY = """
query($owner:String!,$name:String!,$after:String,$n:Int!){
  repository(owner:$owner,name:$name){
    licenseInfo{spdxId}
    pullRequests(states:MERGED,first:$n,after:$after,orderBy:{field:CREATED_AT,direction:ASC}){
      pageInfo{hasNextPage endCursor}
      nodes{
        number title createdAt mergedAt url baseRefOid headRefOid changedFiles
        mergeCommit{oid}
        closingIssuesReferences(first:5){nodes{
          number title body createdAt
          author{login __typename}
          repository{nameWithOwner}
        }}
        files(first:100){nodes{path changeType}}
      }
    }
  }
}
"""


def list_prs(api: Api, repo: str) -> tuple[str, list[dict]]:
    owner, name = repo.split("/")
    after = None
    out: list[dict] = []
    lic = "NOASSERTION"
    page = 0
    while True:
        args = ["graphql", "-f", f"query={PR_QUERY}", "-F", f"owner={owner}", "-F", f"name={name}", "-F", "n=40"]
        if after:
            args += ["-f", f"after={after}"]
        raw = api.get(["prs-v1", repo, after or ""], args)
        data = json.loads(raw)["data"]["repository"]
        lic = (data.get("licenseInfo") or {}).get("spdxId") or "NOASSERTION"
        conn = data["pullRequests"]
        out.extend(conn["nodes"])
        page += 1
        if not conn["pageInfo"]["hasNextPage"]:
            # the last page of a live repository must not be cached past the
            # cutoff: it is, deliberately (re-runs are offline); CUTOFF bounds it
            break
        after = conn["pageInfo"]["endCursor"]
    return lic, [p for p in out if p["mergedAt"] and p["mergedAt"] < CUTOFF]


_COMMENT = re.compile(r"<!--.*?-->", re.S)
_FENCE = re.compile(r"```.*?```|~~~.*?~~~", re.S)
_HEADING = re.compile(r"^\s{0,3}#{1,6} .*$", re.M)
_CHECK = re.compile(r"^\s*[-*] \[[ xX]\].*$", re.M)
_BOLD_LABEL = re.compile(r"^\s*\*\*[^*\n]{1,60}\*\*:?\s*$", re.M)
_LEAK = re.compile(r"```\s*(?:diff|patch)\b|^\+\+\+ b/|^--- a/|^@@ -\d+(?:,\d+)? \+\d+", re.M)


def prose_len(title: str, body: str) -> int:
    t = _COMMENT.sub("", body or "")
    t = _FENCE.sub("", t)
    t = _HEADING.sub("", t)
    t = _CHECK.sub("", t)
    t = _BOLD_LABEL.sub("", t)
    return len(" ".join((title + " " + t).split()))


def strip_binary(diff: str) -> str:
    """Drop the per-file sections of binary files."""
    parts = re.split(r"(?m)^(?=diff --git )", diff)
    keep = [p for p in parts if p and "\nGIT binary patch" not in p and not re.search(r"(?m)^Binary files .* differ$", p)]
    return "".join(keep)


def patch_files(diff: str) -> list[tuple[str | None, str | None]]:
    """(old path or None when created, new path or None when deleted) per file."""
    out = []
    for sec in re.split(r"(?m)^(?=diff --git )", diff):
        if not sec.startswith("diff --git "):
            continue
        head = sec.split("\n", 1)[0]
        old, sep, new = head.removeprefix("diff --git a/").rpartition(" b/")
        if not sep:
            continue
        o: str | None = old
        n: str | None = new
        if re.search(r"(?m)^new file mode", sec) or re.search(r"(?m)^--- /dev/null$", sec):
            o = None
        if re.search(r"(?m)^deleted file mode", sec) or re.search(r"(?m)^\+\+\+ /dev/null$", sec):
            n = None
        out.append((o, n))
    return out


def mine_repo(api: Api, repo: str, lang: str) -> tuple[list[dict], dict]:
    lic, prs = list_prs(api, repo)
    st = {k: 0 for k in ("merged_prs", "closing", "no_issue", "bot", "issue_after", "short", "leak", "no_lang", "not_mostly", "too_many", "truncated", "no_diff", "too_big", "no_lang_patch", "unreachable", "dup_issue", "eligible")}
    st["license"] = lic
    st["merged_prs"] = len(prs)
    rows: list[dict] = []
    for pr in sorted(prs, key=lambda p: (p["mergedAt"], p["number"])):
        issues = [i for i in pr["closingIssuesReferences"]["nodes"] if i and i["repository"]["nameWithOwner"].lower() == repo.lower()]
        if not issues:
            st["no_issue"] += 1
            continue
        st["closing"] += 1
        issues.sort(key=lambda i: i["number"])
        humans = [i for i in issues if (i.get("author") or {}).get("__typename") != "Bot" and not ((i.get("author") or {}).get("login") or "").endswith("[bot]")]
        if not humans:
            st["bot"] += 1
            continue
        issues = humans
        if any(i["createdAt"] >= pr["createdAt"] for i in issues):
            st["issue_after"] += 1
            continue
        if sum(prose_len(i["title"] or "", i["body"] or "") for i in issues) < 100:
            st["short"] += 1
            continue
        text = "\n\n".join(f"{i['title'] or ''}\n{i['body'] or ''}".strip() for i in issues)
        if _LEAK.search(text):
            st["leak"] += 1
            continue
        if pr["changedFiles"] > 100:
            st["truncated"] += 1
            continue
        files = pr["files"]["nodes"] or []
        code = [f for f in files if not is_doc(f["path"], lang) and not is_test(f["path"])]
        lang_all = [f for f in code if is_lang(f["path"], lang)]
        lang_existing = [f for f in lang_all if f["changeType"] not in ("ADDED", "COPIED")]
        if not lang_existing:
            st["no_lang"] += 1
            continue
        if len(lang_all) * 2 <= len(code):
            st["not_mostly"] += 1
            continue
        if len(lang_all) > 10:
            st["too_many"] += 1
            continue
        diff = api.get(["diff-v1", repo, str(pr["number"])], [f"repos/{repo}/pulls/{pr['number']}", "-H", "Accept: application/vnd.github.diff"], ok_fail=True)
        cmp_raw = api.get(["cmp-v1", repo, pr["baseRefOid"], pr["headRefOid"]], [f"repos/{repo}/compare/{pr['baseRefOid']}...{pr['headRefOid']}", "--jq", "{mb:.merge_base_commit.sha,ahead:.ahead_by,n:(.files|length)}"], ok_fail=True)
        if not diff or not cmp_raw:
            st["no_diff"] += 1
            continue
        base = json.loads(cmp_raw).get("mb")
        if not base or not re.fullmatch(r"[0-9a-f]{40}", base):
            st["no_diff"] += 1
            continue
        patch = strip_binary(diff)
        if len(patch.encode()) > 100_000:
            st["too_big"] += 1
            continue
        pf = patch_files(patch)
        gold = [o for o, _ in pf if o and is_lang(o, lang) and not is_test(o)]
        if not gold:
            st["no_lang_patch"] += 1
            continue
        ok = api.get(["commit-v1", repo, base], [f"repos/{repo}/commits/{base}", "--jq", ".sha"], ok_fail=True)
        if not ok or ok.strip() != base:
            st["unreachable"] += 1
            continue
        owner, name = repo.split("/")
        rows.append(
            {
                "instance_id": f"{owner}__{name}-{pr['number']}",
                "repo": repo,
                "base_commit": base,
                "patch": patch,
                "problem_statement": text,
                "language": lang,
                "pr_number": pr["number"],
                "issue_numbers": [i["number"] for i in issues],
                "merge_commit": (pr.get("mergeCommit") or {}).get("oid"),
                "pr_merged_at": pr["mergedAt"],
                "issue_created_at": min(i["createdAt"] for i in issues),
                "license": lic,
                "pr_url": pr["url"],
                "gold_files": sorted(set(gold)),
            }
        )
    # one PR per issue: the earliest merged wins
    seen: set[int] = set()
    kept = []
    for r in sorted(rows, key=lambda r: (r["pr_merged_at"], r["pr_number"])):
        if any(n in seen for n in r["issue_numbers"]):
            st["dup_issue"] += 1
            continue
        seen.update(r["issue_numbers"])
        kept.append(r)
    st["eligible"] = len(kept)
    return kept, st


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--lang", required=True, choices=sorted(EXT))
    ap.add_argument("--repos", nargs="+", required=True)
    ap.add_argument("--cache", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--stats", required=True, type=Path)
    a = ap.parse_args()
    api = Api(a.cache)
    rows: list[dict] = []
    stats: dict[str, dict] = {}
    for repo in a.repos:
        try:
            r, st = mine_repo(api, repo, a.lang)
        except Exception as e:  # noqa: BLE001 - a repository that cannot be listed is reported, not fatal
            print(f"{repo}: FAILED {e}", file=sys.stderr)
            stats[repo] = {"error": str(e)[:300]}
            continue
        stats[repo] = st
        rows.extend(r)
        print(f"{repo}: merged={st['merged_prs']} closing={st['closing']} eligible={st['eligible']} (api calls so far {api.calls})", file=sys.stderr)
    rows.sort(key=lambda r: r["instance_id"])
    a.out.parent.mkdir(parents=True, exist_ok=True)
    with a.out.open("w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")
    a.stats.write_text(json.dumps(stats, indent=1, sort_keys=True) + "\n")
    print(hashlib.sha256(a.out.read_bytes()).hexdigest(), a.out, len(rows))


if __name__ == "__main__":
    main()
