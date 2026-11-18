#!/usr/bin/env bash
# Reproducible organization demo: Rueca over the tokio-rs family of
# repositories (tokio-rs, hyperium and tower-rs GitHub organizations).
#
# Usage: scripts/demo-tokio-rs.sh [all|clone|index|queries|checks|clean [--all]]
#
#   all      (default) clone, index, queries, checks, summary
#   clone    clone the 8 repositories (skipped when present) and check out
#            the pinned commits; the only network use of the demo
#   index    write the organization file, index it, record status, links
#            and contracts
#   queries  run the showcase queries (out/NN-<slug>.txt, out/timings.tsv)
#   checks   measure the known weak spots (out/checks.txt)
#   clean    remove the store and out/; `clean --all` removes the clones too
#
# Environment:
#   RUECA_DEMO_CACHE  working directory (default: ${TMPDIR:-/tmp}/rueca-demo-cache)
#   RUECA             rueca binary (default: `rueca` on PATH)
#   RUECA_HOME        store (default: $RUECA_DEMO_CACHE/store)
#
# Needs git, jq, perl (timings) and python3 (one estimate in `checks`).

set -euo pipefail

tmp_base=${TMPDIR:-/tmp}
RUECA_DEMO_CACHE=${RUECA_DEMO_CACHE:-${tmp_base%/}/rueca-demo-cache}
RUECA=${RUECA:-rueca}
RUECA_HOME=${RUECA_HOME:-$RUECA_DEMO_CACHE/store}
export RUECA_HOME

REPOS_DIR=$RUECA_DEMO_CACHE/repos
OUT=$RUECA_DEMO_CACHE/out
ORG_FILE=$RUECA_DEMO_CACHE/rueca-org.toml
ORG=tokio-rs

# Pins: GitHub org/repo, tag, commit. The latest release of each main crate
# as of 2026-11-17.
PINS=(
  "tokio-rs/tokio   tokio-1.54.1    b53b2001ed2ccfd9b87e32a0c6da1a1eebdc80b8"
  "tokio-rs/mio     v1.2.5          101e5b7c92b1b5f8abc0ba3eb81b264943e13e37"
  "tokio-rs/bytes   v1.12.2         67edc2b999d669eb4f471aff576e213dbcb58871"
  "tokio-rs/tracing tracing-0.1.44  2d55f6faf9be83e7e4634129fb96813241aac2b8"
  "hyperium/hyper   v1.12.0         7f67699ce32d717081deb7018b4da7d679277ac0"
  "tower-rs/tower   tower-0.5.3     4b0a6b0e688bd177eb2c9c97f5268dd9703c66fc"
  "tokio-rs/axum    axum-v0.8.9     c59208c86fded335cd85e388030ad59347b0e5ae"
  "hyperium/tonic   tonic-v0.14.9   4c0f455c264fe2ec9f9ab5d0cc7fd2824d5daf70"
)
# The second ref of tokio: the release before (RFC 0006 §9.3).
TOKIO_PREV_TAG=tokio-1.53.3
TOKIO_PREV_COMMIT=a85c3f5baa298a6b554fd3700daf0342289881af

TOKIO=github.com/tokio-rs/tokio

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
now_ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time() * 1000'; }

need() {
  command -v "$1" >/dev/null 2>&1 || die "$1 not found${2:+ ($2)}"
}

# ---------------------------------------------------------------- clone

verify_tag() { # dir tag commit
  local got
  got=$(git -C "$1" rev-parse --verify --quiet "$2^{commit}") \
    || die "$1: tag $2 not found"
  [[ $got == "$3" ]] || die "$1: tag $2 is $got, expected $3 (the tag moved)"
}

cmd_clone() {
  need git
  mkdir -p "$REPOS_DIR"
  local pin slug tag commit name dir
  for pin in "${PINS[@]}"; do
    read -r slug tag commit <<<"$pin"
    name=${slug#*/}
    dir=$REPOS_DIR/$name
    if [[ -d $dir/.git ]]; then
      log "clone $slug: present"
    else
      log "clone $slug"
      git clone --quiet "https://github.com/$slug.git" "$dir"
    fi
    verify_tag "$dir" "$tag" "$commit"
    git -C "$dir" -c advice.detachedHead=false checkout --quiet --detach "$commit"
    [[ $(git -C "$dir" rev-parse HEAD) == "$commit" ]] || die "$dir: checkout failed"
  done
  verify_tag "$REPOS_DIR/tokio" "$TOKIO_PREV_TAG" "$TOKIO_PREV_COMMIT"
}

# ---------------------------------------------------------------- index

write_org_file() {
  local pin slug tag commit name
  {
    printf 'name = "%s"\n' "$ORG"
    for pin in "${PINS[@]}"; do
      read -r slug tag commit <<<"$pin"
      name=${slug#*/}
      printf '\n[[repo]]\npath = "%s"\nid = "github.com/%s"\n' "$REPOS_DIR/$name" "$slug"
      if [[ $name == tokio ]]; then
        printf 'refs = ["%s"]\n' "$TOKIO_PREV_TAG"
      fi
    done
  } >"$ORG_FILE"
}

cmd_index() {
  need "$RUECA" "set RUECA to the binary"
  need jq
  [[ -d $REPOS_DIR/tokio ]] || die "no clones; run: $0 clone"
  mkdir -p "$OUT"
  write_org_file
  "$RUECA" --version >"$OUT/version.txt"

  local t0 t1
  log "index --org (JSON: out/index.json)"
  t0=$(now_ms)
  "$RUECA" index --org "$ORG_FILE" --json >"$OUT/index.json"
  t1=$(now_ms)
  echo "$((t1 - t0))" >"$OUT/index-wall-ms.txt"
  du -sh "$RUECA_HOME" | cut -f1 >"$OUT/store-size-first.txt"

  log "index --org again (human summary: out/index-again.txt)"
  t0=$(now_ms)
  "$RUECA" index --org "$ORG_FILE" >"$OUT/index-again.txt"
  t1=$(now_ms)
  echo "$((t1 - t0))" >"$OUT/index-again-wall-ms.txt"

  "$RUECA" status >"$OUT/status.txt"
  du -sh "$RUECA_HOME" | cut -f1 >"$OUT/store-size.txt"

  log "links and contracts"
  "$RUECA" links --org "$ORG" >"$OUT/links.txt"
  "$RUECA" links --org "$ORG" --json >"$OUT/links.json"
  {
    echo "# edges: $(jq '.edges | length' "$OUT/links.json")"
    echo "# per consumer -> provider"
    jq -r '.edges | group_by([.consumer, .dst_repo])
      | map([length, .[0].consumer, .[0].dst_repo]) | sort_by(-.[0]) | .[]
      | "\(.[0])\t\(.[1]) -> \(.[2])"' "$OUT/links.json"
    echo "# per kind"
    jq -r '.edges | group_by(.kind) | map([length, .[0].kind]) | sort_by(-.[0]) | .[]
      | "\(.[0])\t\(.[1])"' "$OUT/links.json"
    echo "# per via / strategy"
    jq -r '.edges | group_by([.via, .strategy]) | map([length, .[0].via, .[0].strategy])
      | sort_by(-.[0]) | .[] | "\(.[0])\t\(.[1]) \(.[2])"' "$OUT/links.json"
  } >"$OUT/links-summary.txt"

  "$RUECA" contracts --org "$ORG" >"$OUT/contracts.txt"
  "$RUECA" contracts --org "$ORG" --side dangling >"$OUT/contracts-dangling.txt"
  "$RUECA" contracts --org "$ORG" --kind rpc >"$OUT/contracts-rpc.txt"
  "$RUECA" contracts --org "$ORG" --json >"$OUT/contracts.json"
  {
    echo "# contract rows by kind / side / framework"
    jq -r '.results | group_by([.kind, .side, .framework])
      | map([length, .[0].kind, .[0].side, .[0].framework]) | .[]
      | "\(.[0])\t\(.[1]) \(.[2]) \(.[3])"' "$OUT/contracts.json"
    echo "# rows with at least one counterpart: $(jq '[.results[] | select(.matches > 0)] | length' "$OUT/contracts.json")"
    echo "# dangling consumers: $(grep -c '  consumes  ' "$OUT/contracts-dangling.txt" || true)"
  } >"$OUT/contracts-summary.txt"
}

# ---------------------------------------------------------------- queries

STEPS=0

# step NN slug tool 'json': the exact command on the first line, then its
# output (stdout and stderr; an error is output too); the median of 3 runs
# goes to timings.tsv.
step() {
  local nn=$1 slug=$2 tool=$3 args=$4
  local file=$OUT/$nn-$slug.txt t0 t1 i
  local -a ms=()
  log "$nn $slug"
  for i in 1 2 3; do
    t0=$(now_ms)
    {
      printf "# rueca query %s '%s' --org %s --text\n" "$tool" "$args" "$ORG"
      "$RUECA" query "$tool" "$args" --org "$ORG" --text 2>&1 || true
    } >"$file.tmp"
    t1=$(now_ms)
    ms+=("$((t1 - t0))")
    mv "$file.tmp" "$file"
    : "$i"
  done
  printf '%s\t%s\n' "$nn-$slug" \
    "$(printf '%s\n' "${ms[@]}" | sort -n | sed -n 2p)" >>"$OUT/timings.tsv"
  STEPS=$((STEPS + 1))
}

cmd_queries() {
  need "$RUECA" "set RUECA to the binary"
  [[ -f $OUT/index.json ]] || die "not indexed; run: $0 index"
  rm -f "$OUT"/[0-9][0-9]-*.txt
  printf 'step\tmedian_ms\n' >"$OUT/timings.tsv"
  local prev=$TOKIO_PREV_TAG

  # Repository routing.
  step 01 list-repositories list_repositories '{}'
  step 02 route-http-router list_repositories '{"query":"http router"}'
  step 03 route-async-runtime list_repositories '{"query":"async runtime"}'

  # One ranking over the organization, then narrowed with `repo`.
  step 04 search-graceful-shutdown search_symbols '{"query":"graceful shutdown","limit":8}'
  step 05 search-decode-frame search_symbols '{"query":"decode frame","limit":8}'
  step 06 search-decode-frame-tokio search_symbols \
    '{"query":"decode frame","repo":"github.com/tokio-rs/tokio","limit":6}'
  step 07 search-decode-frame-tonic-tokio search_symbols \
    '{"query":"decode frame","repo":["github.com/hyperium/tonic","github.com/tokio-rs/tokio"],"limit":8}'

  # Cross-repository call graph.
  step 08 trace-mio-poll-callers trace \
    '{"qualified_name":"mio::poll::Poll.poll","direction":"callers","depth":4}'
  step 09 trace-bytes-copy-from-slice-callers trace \
    '{"qualified_name":"bytes::bytes::Bytes.copy_from_slice","direction":"callers","limit":15}'
  step 10 trace-tokio-watch-channel-callers trace \
    '{"qualified_name":"tokio::sync::watch::channel","direction":"callers","limit":12}'
  step 11 trace-tokio-io-driver-callees trace \
    '{"qualified_name":"tokio::runtime::io::driver::Driver.turn","direction":"callees"}'
  step 12 trace-servicebuilder-layer-repo trace \
    '{"qualified_name":"ServiceBuilder.layer","repo":"github.com/tower-rs/tower","direction":"callers","min_confidence":0.85,"limit":10}'

  # Two refs of tokio.
  step 13 refs-compare-blocking search_symbols \
    "{\"query\":\"blocking acquire lock\",\"repo\":\"$TOKIO\",\"ref\":[\"HEAD\",\"$prev\"],\"limit\":8}"
  step 14 refs-previous-release search_symbols \
    "{\"query\":\"blocking acquire lock\",\"repo\":\"$TOKIO\",\"ref\":\"$prev\",\"limit\":5}"

  # Contracts.
  step 15 contracts-greeter list_contracts '{"kind":"rpc","query":"Greeter/SayHello","limit":12}'
  step 16 contracts-dangling list_contracts '{"side":"dangling","limit":6}'

  # Symbol and file views.
  step 17 get-symbol-steal-work get_symbol \
    '{"qualified_name":"tokio::runtime::scheduler::multi_thread::worker::Core.steal_work"}'
  step 18 outline-io-driver get_file_outline \
    "{\"path\":\"tokio/src/runtime/io/driver.rs\",\"repo\":\"$TOKIO\"}"

  # Natural-language questions (lexical ranking).
  step 19 nl-axum-handler-service search_symbols \
    '{"query":"where does axum turn a handler into a service","limit":6}'
  step 20 nl-tokio-steal-work search_symbols \
    '{"query":"how does tokio steal work from other workers","limit":6}'
  step 21 nl-tonic-prost-encode search_symbols \
    '{"query":"how does tonic encode a message with prost","limit":6}'
  step 22 nl-tokio-worker-thread search_symbols \
    '{"query":"how does tokio decide which worker thread runs a task","limit":6}'
  step 23 nl-hyper-request-body search_symbols \
    '{"query":"how does hyper read the request body","limit":6}'

  # Text search and budgets.
  step 24 search-code-cfg-net search_code \
    '{"pattern":"cfg_net!\\s*\\{","regex":true,"language":"rust","limit":8}'
  step 25 trace-oneshot-budget trace \
    '{"qualified_name":"tokio::sync::oneshot::channel","direction":"callers","budget_tokens":400}'
}

# ---------------------------------------------------------------- checks

q_text() { "$RUECA" query "$1" "$2" --org "$ORG" --text 2>&1 || true; }

callers_count() { # qualified name -> number of callers (tests excluded)
  "$RUECA" query trace "{\"qualified_name\":\"$1\",\"direction\":\"callers\",\"limit\":2000}" \
    --org "$ORG" 2>/dev/null | jq '.results.nodes | length - 1' 2>/dev/null || echo error
}

cfg_estimate() {
  python3 -I - "$1" <<'PY'
import bisect, os, re, sys
start = re.compile(r'\bcfg_[a-z0-9_]+!\s*\{')
item = re.compile(r'^\s*pub(\([^)]*\))?\s+(async\s+|unsafe\s+|const\s+)*(fn|struct|trait|enum|use|mod|type)\b')
blocks = total = inside = 0
kinds = {}
for dp, _, fns in sorted(os.walk(sys.argv[1])):
    for f in sorted(fns):
        if not f.endswith('.rs'):
            continue
        src = open(os.path.join(dp, f), encoding='utf-8', errors='replace').read()
        lines = src.split('\n')
        offs = [0]
        for l in lines:
            offs.append(offs[-1] + len(l) + 1)
        mask = [False] * len(lines)
        for m in start.finditer(src):
            i, depth = m.end(), 1
            while i < len(src) and depth:
                depth += {'{': 1, '}': -1}.get(src[i], 0)
                i += 1
            a = bisect.bisect_right(offs, m.start()) - 1
            b = bisect.bisect_right(offs, i) - 1
            if mask[a]:
                continue  # nested in an outer block
            blocks += 1
            for k in range(a + 1, b):
                mask[k] = True
        for k, l in enumerate(lines):
            m = item.match(l)
            if m:
                total += 1
                kd = kinds.setdefault(m.group(3), [0, 0])
                kd[1] += 1
                if mask[k]:
                    inside += 1
                    kd[0] += 1
print(f"outermost cfg_*! {{ }} bodies: {blocks}")
print(f"pub(...) fn/struct/trait/enum/use/mod/type lines: {total}, inside cfg_*! bodies: {inside} ({100 * inside / total:.1f}%)")
print("by kind (inside / all): " + ", ".join(f"{k} {v[0]}/{v[1]}" for k, v in sorted(kinds.items())))
PY
}

cmd_checks() {
  need "$RUECA" "set RUECA to the binary"
  need jq
  [[ -f $OUT/links.json ]] || die "not indexed; run: $0 index"
  local f=$OUT/checks.txt r m t name path where spec
  log "checks (out/checks.txt)"
  {
    echo "## 1. Attribute macros: #[tokio::main] / #[tokio::test] vs link edges into tokio-macros"
    for r in axum hyper tonic tower tracing; do
      m=$(grep -rE --include='*.rs' '#\[tokio::main' "$REPOS_DIR/$r" | wc -l | tr -d ' ')
      t=$(grep -rE --include='*.rs' '#\[tokio::test' "$REPOS_DIR/$r" | wc -l | tr -d ' ')
      echo "$r: #[tokio::main] $m, #[tokio::test] $t"
    done
    echo "link edges into tokio-macros: $(jq '[.edges[] | select(.dst_path | startswith("tokio-macros/"))] | length' "$OUT/links.json")"
    echo
    echo "## 2. Items inside cfg_*! { ... } bodies (tokio/src)"
    echo "cfg_*! { occurrences (grep): $(grep -rhoE 'cfg_[a-z0-9_]+! *\{' "$REPOS_DIR/tokio/tokio/src" | wc -l | tr -d ' ')"
    cfg_estimate "$REPOS_DIR/tokio/tokio/src"
    echo "get_symbol name + path (first line of the answer):"
    for spec in \
      'TcpListener|tokio/src/net/tcp/listener.rs|inside cfg_net!' \
      'UdpSocket|tokio/src/net/udp.rs|inside cfg_net!' \
      'spawn|tokio/src/task/spawn.rs|inside cfg_rt!' \
      'AsyncReadExt|tokio/src/io/util/async_read_ext.rs|inside cfg_io_util!' \
      'AsyncReadExt.read|tokio/src/io/util/async_read_ext.rs|inside cfg_io_util!' \
      'File|tokio/src/fs/file.rs|outside' \
      'channel|tokio/src/sync/mpsc/bounded.rs|outside'; do
      IFS='|' read -r name path where <<<"$spec"
      echo "  $name ($where): $(q_text get_symbol "{\"name\":\"$name\",\"path\":\"$path\",\"repo\":\"$TOKIO\"}" | grep -v '^github.com/' | head -1 | cut -c1-100)"
    done
    echo
    echo "## 3. tokio -> mio edges (same-named types: TcpStream, UdpSocket, UnixStream, ...)"
    jq -r '[.edges[] | select(.consumer == "github.com/tokio-rs/tokio" and .dst_repo == "github.com/tokio-rs/mio")]
      | "edges: \(length)", (group_by([.kind, .strategy]) | .[] | "  \(.[0].kind) \(.[0].strategy): \(length)")' "$OUT/links.json"
    echo "Edges whose source lies in a dependency of the consumer (right for impl LocalTrait for ForeignType,"
    echo "wrong where a local type was resolved to a same-named type of another crate):"
    jq -r '.edges[] | select(.src_repo != .consumer)
      | "  \(.src) -> \(.dst) \(.kind) @\(.path):\(.line)"' "$OUT/links.json"
    echo "  hyper::ext::h1_reason_phrase::Bytes.from as a call target (impl From<ReasonPhrase> for Bytes): $(jq '[.edges[] | select(.dst == "hyper::ext::h1_reason_phrase::Bytes.from" and .kind == "CALLS")] | length' "$OUT/links.json") edges"
    echo
    echo "## 4. Functions re-exported from a same-named module (mod sleep; pub use sleep::sleep)"
    for name in tokio::time::sleep::sleep tokio::time::timeout::timeout tokio::time::interval::interval \
      tokio::time::instant::Instant.now tokio::sync::watch::channel; do
      echo "  callers of $name: $(callers_count "$name")"
    done
    for name in sleep timeout interval; do
      echo "  time::$name( call sites in axum/hyper/tonic/tower/tracing: $(grep -rE --include='*.rs' "(tokio::)?time::$name\(" \
        "$REPOS_DIR/axum" "$REPOS_DIR/hyper" "$REPOS_DIR/tonic" "$REPOS_DIR/tower" "$REPOS_DIR/tracing" | wc -l | tr -d ' ')"
    done
    echo
    echo "## 5. Errors reported by the index"
    jq -r '"link_errors: \(.update.link_errors | length), ref_errors: \(.ref_errors | length)",
      (.indexed[] | "  \(.segment.snapshot.repo): skipped \(.files_skipped), partial \(.files_partial), unsupported \(.files_unsupported)")' \
      "$OUT/index.json"
    echo
    echo "## 6. The default ref of a detached checkout is named HEAD, not by its tag"
    q_text search_symbols "{\"query\":\"spawn\",\"repo\":\"$TOKIO\",\"ref\":\"tokio-1.54.1\",\"limit\":1}"
  } >"$f"
}

# ---------------------------------------------------------------- summary

summary() {
  local edges link_edges
  edges=$(jq '.edges | length' "$OUT/links.json")
  link_edges=$(jq '.update.link_edges' "$OUT/index.json")
  cat <<EOF

Rueca organization demo: $ORG ($("$RUECA" --version))
  index (first run):   $(cat "$OUT/index-wall-ms.txt") ms wall, $(jq '.duration_ms' "$OUT/index.json") ms reported (repos $(jq '.repos_ms' "$OUT/index.json"), refs $(jq '.refs_ms' "$OUT/index.json"), org $(jq '.org_ms' "$OUT/index.json"))
  index (second run):  $(cat "$OUT/index-again-wall-ms.txt") ms wall
  store:               $(cat "$OUT/store-size-first.txt") after the first run, $(cat "$OUT/store-size.txt") after the second ($RUECA_HOME)
  link edges:          $edges between default refs ($link_edges with the tokio $TOKIO_PREV_TAG entry)
  contracts:           $(wc -l <"$OUT/contracts.txt" | tr -d ' ') lines, $(grep -c '  consumes  ' "$OUT/contracts-dangling.txt" || true) dangling
  queries:             $STEPS (median ms in out/timings.tsv)
  outputs:             $OUT
EOF
}

# ---------------------------------------------------------------- main

cmd_clean() {
  log "remove $RUECA_HOME and $OUT"
  rm -rf "$RUECA_HOME" "$OUT"
  if [[ ${1:-} == --all ]]; then
    log "remove $REPOS_DIR and $ORG_FILE"
    rm -rf "$REPOS_DIR" "$ORG_FILE"
  fi
}

main() {
  local cmd=${1:-all}
  case $cmd in
    all) cmd_clone; cmd_index; cmd_queries; cmd_checks; summary ;;
    clone) cmd_clone ;;
    index) cmd_index ;;
    queries) cmd_queries ;;
    checks) cmd_checks ;;
    clean) shift; cmd_clean "$@" ;;
    -h | --help | help) sed -n '2,22p' "$0" ;;
    *) die "unknown command: $cmd (all, clone, index, queries, checks, clean)" ;;
  esac
}

main "$@"
