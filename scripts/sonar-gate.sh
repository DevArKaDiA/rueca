#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# Rueca's SonarQube quality gate, run against a disposable SonarQube
# Community server (a service container in CI, a local container otherwise).
#
#   1. waits for the server to be UP;
#   2. configures it through the Web API: admin password, project, the
#      `rueca-strict` quality profiles (Rust: every rule; Python: the default
#      built-in profile plus a few rules we insist on) and the `rueca-strict`
#      quality gate on overall code;
#   3. runs sonar-scanner (Docker image, pinned) with sonar-project.properties;
#   4. waits for the server to process the report, prints a summary (gate
#      conditions, metrics, issues by rule and file, hotspots) and exits
#      non-zero when the gate fails.
#
# Usage:  scripts/sonar-gate.sh [extra sonar-scanner arguments]
#   e.g.  SONAR_HOST_URL=http://localhost:9100 scripts/sonar-gate.sh -Dsonar.sources=bench
#
# A local throwaway server (same image as CI, see .github/workflows/sonarqube.yml;
# the tmpfs keeps Elasticsearch off a nearly full Docker disk):
#   docker run -d --name rueca-sonar-ci -p 9100:9000 \
#     --tmpfs /opt/sonarqube/data:rw,size=3g,mode=1777 sonarqube:community@sha256:...
#   ... run the gate ...; docker rm -f rueca-sonar-ci
#
# Environment:
#   SONAR_HOST_URL          server URL (default http://localhost:9000)
#   SONAR_ADMIN_PASSWORD    admin password: set on a fresh server (admin/admin),
#                           used as is on an already configured one
#   SONAR_SCANNER_HOST_URL  server URL as seen from the scanner container
#                           (default: SONAR_HOST_URL; on macOS localhost is
#                           rewritten to host.docker.internal)
#   SONAR_SCANNER_IMAGE     sonar-scanner image (default: pinned below)
#   SONAR_WAIT_SECONDS      how long to wait for the server and the report (300)
#   GITHUB_STEP_SUMMARY     if set, the Markdown summary is appended to it
#
# Requires: POSIX sh, curl, jq, docker. Meant for a throwaway server: it
# changes the admin password and creates profiles and a gate.

set -eu

HOST=${SONAR_HOST_URL:-http://localhost:9000}
HOST=${HOST%/}
PASSWORD=${SONAR_ADMIN_PASSWORD:-Rueca-ephemeral-gate-1}
# sonarsource/sonar-scanner-cli 12.1 (SonarScanner CLI 8.1.0.6389), linux/amd64.
SCANNER_IMAGE=${SONAR_SCANNER_IMAGE:-sonarsource/sonar-scanner-cli@sha256:47956b669bf486bdad919e1cbd0533223515aa0b6d1d31763e5a3754942df6d7}
WAIT=${SONAR_WAIT_SECONDS:-300}
PROJECT=rueca
PROFILE=rueca-strict
GATE=rueca-strict
# Python rules that must stay active on top of the built-in profile.
PYTHON_RULES="S5443 S8786 S1244 S2245"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 2; }

for tool in curl jq docker; do
  command -v "$tool" >/dev/null || die "$tool is required"
done

# --- Web API helpers -------------------------------------------------------

# get PATH [curl args]: authenticated GET of $HOST/api/PATH.
get() {
  path=$1; shift
  curl -sS --fail-with-body -u "admin:$PASSWORD" -G "$HOST/api/$path" "$@"
}

# post PATH name=value...: authenticated POST with url-encoded parameters.
post() {
  url="$HOST/api/$1"; shift
  n=$#
  while [ "$n" -gt 0 ]; do # rewrite each name=value as --data-urlencode name=value
    set -- "$@" --data-urlencode "$1"
    shift
    n=$((n - 1))
  done
  out=$(curl -sS --fail-with-body -u "admin:$PASSWORD" -X POST "$url" "$@") || die "POST $url: $out"
}

# --- 1. server -------------------------------------------------------------

wait_up() {
  log "waiting for $HOST to be UP"
  waited=0
  while :; do
    status=$(curl -s "$HOST/api/system/status" | jq -r '.status // empty' 2>/dev/null || true)
    case $status in
      UP) return ;;
      DOWN | DB_MIGRATION_NEEDED | DB_MIGRATION_RUNNING) ;;
    esac
    [ "$waited" -ge "$WAIT" ] && die "server not UP after ${WAIT}s (status: ${status:-unreachable})"
    sleep 5
    waited=$((waited + 5))
  done
}

valid_login() {
  curl -s -u "admin:$1" "$HOST/api/authentication/validate" | jq -e '.valid' >/dev/null 2>&1
}

login() {
  if valid_login "$PASSWORD"; then
    return
  fi
  valid_login admin || die "cannot log in as admin; set SONAR_ADMIN_PASSWORD"
  log "setting the admin password"
  curl -sS --fail-with-body -u admin:admin -X POST "$HOST/api/users/change_password" \
    --data-urlencode login=admin --data-urlencode previousPassword=admin \
    --data-urlencode "password=$PASSWORD" >/dev/null
}

# --- 2. project, profiles, gate --------------------------------------------

ensure_project() {
  if [ "$(get projects/search --data-urlencode "projects=$PROJECT" | jq '.paging.total')" = 0 ]; then
    log "creating project $PROJECT"
    post projects/create "project=$PROJECT" "name=Rueca"
  fi
}

# profile_key LANG NAME: key of the quality profile, empty if none.
profile_key() {
  get qualityprofiles/search --data-urlencode "language=$1" |
    jq -r --arg n "$2" '.profiles[] | select(.name == $n) | .key'
}

# Rust: every rule the analyzer has (incl. S3776 cognitive complexity, 15).
rust_profile() {
  key=$(profile_key rust "$PROFILE")
  if [ -z "$key" ]; then
    log "creating Rust profile $PROFILE (all rules)"
    post qualityprofiles/create "language=rust" "name=$PROFILE"
    key=$(profile_key rust "$PROFILE")
  fi
  post qualityprofiles/activate_rules "targetKey=$key" "languages=rust" "qprofile=$key" "activation=false"
  # explicit, so a change of the analyzer's default cannot loosen it
  post qualityprofiles/activate_rule "key=$key" "rule=rust:S3776" "params=threshold=15"
  post qualityprofiles/add_project "language=rust" "qualityProfile=$PROFILE" "project=$PROJECT"
}

# Python: a copy of the built-in default profile ("Sonar way") plus PYTHON_RULES.
python_profile() {
  key=$(profile_key py "$PROFILE")
  if [ -z "$key" ]; then
    base=$(get qualityprofiles/search --data-urlencode language=py |
      jq -r '.profiles[] | select(.isBuiltIn and .isDefault) | .key')
    log "creating Python profile $PROFILE (built-in default + $PYTHON_RULES)"
    post qualityprofiles/copy "fromKey=$base" "toName=$PROFILE"
    key=$(profile_key py "$PROFILE")
  fi
  for rule in $PYTHON_RULES; do
    post qualityprofiles/activate_rule "key=$key" "rule=python:$rule"
  done
  post qualityprofiles/add_project "language=py" "qualityProfile=$PROFILE" "project=$PROJECT"
}

# The server is ephemeral, so every line is new: the gate is on overall code.
# The server runs in Multi-Quality Rule mode, so bugs / vulnerabilities / code
# smells are the reliability / security / maintainability issue counts. A
# hotspot count cannot be a condition: "100% reviewed" means none is open
# (no hotspots, no value, condition passes).
gate() {
  if get qualitygates/show --data-urlencode "name=$GATE" >/dev/null 2>&1; then
    post qualitygates/destroy "name=$GATE"   # recreate: the conditions below win
  fi
  log "creating quality gate $GATE"
  post qualitygates/create "name=$GATE"
  # a new gate comes with the "Clean as You Code" conditions on new code: drop them
  for id in $(get qualitygates/show --data-urlencode "name=$GATE" | jq -r '.conditions[].id'); do
    post qualitygates/delete_condition "id=$id"
  done
  while read -r metric op error; do
    post qualitygates/create_condition "gateName=$GATE" "metric=$metric" "op=$op" "error=$error"
  done <<EOF
violations GT 0
software_quality_reliability_issues GT 0
software_quality_security_issues GT 0
software_quality_maintainability_issues GT 0
security_hotspots_reviewed LT 100
duplicated_lines_density GT 3
software_quality_reliability_rating GT 1
software_quality_security_rating GT 1
software_quality_maintainability_rating GT 1
EOF
  post qualitygates/select "gateName=$GATE" "projectKey=$PROJECT"
}

# --- 3. scan ---------------------------------------------------------------

scan() {
  SONAR_TOKEN=$(curl -sS --fail-with-body -u "admin:$PASSWORD" -X POST "$HOST/api/user_tokens/generate" \
    --data-urlencode "name=sonar-gate-$(date +%s)-$$" | jq -r '.token')
  export SONAR_TOKEN

  scanner_host=${SONAR_SCANNER_HOST_URL:-$HOST}
  network=host
  if [ "$(uname -s)" = Darwin ] && [ -z "${SONAR_SCANNER_HOST_URL:-}" ]; then
    # Docker on macOS runs in a VM: reach the host through host.docker.internal.
    scanner_host=$(printf '%s' "$HOST" | sed -E 's#//(localhost|127\.0\.0\.1)#//host.docker.internal#')
    network=bridge
  fi

  # The scanner's work files go to the container's /tmp: the checkout may not
  # be writable by its user (uid 1000), and stays clean.
  log "scanning $ROOT with $SCANNER_IMAGE"
  docker run --rm --platform linux/amd64 --network "$network" \
    -e SONAR_HOST_URL="$scanner_host" -e SONAR_TOKEN \
    -v "$ROOT:/usr/src" "$SCANNER_IMAGE" \
    -Dsonar.working.directory=/tmp/scannerwork "$@" 2>&1 | tee "$WORK/scanner.log" >&2

  task=$(sed -n 's#.*api/ce/task?id=\([A-Za-z0-9_-]*\).*#\1#p' "$WORK/scanner.log" | tail -n 1)
  [ -n "$task" ] || die "sonar-scanner failed (no report submitted)"
  printf '%s' "$task"
}

# --- 4. result -------------------------------------------------------------

# wait_task ID: waits for the background task, prints its analysis id.
wait_task() {
  log "waiting for the server to process the report ($1)"
  waited=0
  while :; do
    get ce/task --data-urlencode "id=$1" >"$WORK/task.json"
    status=$(jq -r '.task.status' "$WORK/task.json")
    case $status in
      SUCCESS) jq -r '.task.analysisId' "$WORK/task.json"; return ;;
      FAILED | CANCELED) die "report processing $status: $(jq -r '.task.errorMessage // ""' "$WORK/task.json")" ;;
    esac
    [ "$waited" -ge "$WAIT" ] && die "report not processed after ${WAIT}s"
    sleep 2
    waited=$((waited + 2))
  done
}

METRICS=ncloc,violations,software_quality_reliability_issues,software_quality_security_issues,software_quality_maintainability_issues,security_hotspots,duplicated_lines_density,software_quality_reliability_rating,software_quality_security_rating,software_quality_maintainability_rating

# summary ANALYSIS: Markdown report on stdout; returns the gate status.
summary() {
  get qualitygates/project_status --data-urlencode "analysisId=$1" >"$WORK/gate.json"
  get measures/component --data-urlencode "component=$PROJECT" --data-urlencode "metricKeys=$METRICS" >"$WORK/measures.json"
  get issues/search --data-urlencode "components=$PROJECT" --data-urlencode resolved=false \
    --data-urlencode ps=100 --data-urlencode s=FILE_LINE --data-urlencode facets=rules,files >"$WORK/issues.json"
  : >"$WORK/rules.tsv"
  for rule in $(jq -r '.facets[] | select(.property == "rules") | .values[].val' "$WORK/issues.json"); do
    get rules/show --data-urlencode "key=$rule" | jq -r '[.rule.key, .rule.name] | @tsv' >>"$WORK/rules.tsv"
  done
  get hotspots/search --data-urlencode "project=$PROJECT" --data-urlencode status=TO_REVIEW --data-urlencode ps=100 >"$WORK/hotspots.json"

  status=$(jq -r '.projectStatus.status' "$WORK/gate.json")
  total=$(jq -r '.total' "$WORK/issues.json")
  hotspots=$(jq -r '.paging.total' "$WORK/hotspots.json")

  if [ "$status" = OK ]; then echo "## SonarQube quality gate: passed"; else echo "## SonarQube quality gate: FAILED"; fi
  echo
  echo "Gate \`$GATE\` on overall code. $total open issues, $hotspots security hotspots to review."
  echo
  echo "| Condition | Actual | Threshold | Status |"
  echo "|---|---:|---:|---|"
  jq -r '
    def show($m): if ($m | endswith("_rating")) and . != null
      then "-ABCDE"[(tonumber | floor):][:1] else (. // "-") end;
    .projectStatus.conditions[] | .metricKey as $k |
    "| \($k) | \(.actualValue | show($k)) | \(if .comparator == "GT" then "≤" else "≥" end) \(.errorThreshold | show($k)) | \(.status) |"' "$WORK/gate.json"
  echo
  echo "Metrics: $(jq -r '[.component.measures | sort_by(.metric)[] | "\(.metric)=\(.value)"] | join(", ")' "$WORK/measures.json")"

  if [ "$total" -gt 0 ]; then
    echo
    echo "### Issues by rule"
    echo
    echo "| Rule | Name | Issues |"
    echo "|---|---|---:|"
    jq -r --rawfile r "$WORK/rules.tsv" '
      ($r | split("\n") | map(select(. != "") | split("\t") | {(.[0]): .[1]}) | add // {}) as $names |
      .facets[] | select(.property == "rules") | .values[] |
      "| \(.val) | \($names[.val] // "") | \(.count) |"' "$WORK/issues.json"
    echo
    echo "### Issues by file"
    echo
    echo "| File | Issues |"
    echo "|---|---:|"
    jq -r '.facets[] | select(.property == "files") | .values[] | "| \(.val) | \(.count) |"' "$WORK/issues.json"
    echo
    echo "<details><summary>First $(jq '.issues | length' "$WORK/issues.json") issues</summary>"
    echo
    jq -r '.issues[] | "- `\(.component | sub("^[^:]*:"; "")):\(.line // 0)` \(.rule): \(.message)"' "$WORK/issues.json"
    echo
    echo "</details>"
  fi

  if [ "$hotspots" -gt 0 ]; then
    echo
    echo "### Security hotspots"
    echo
    jq -r '.hotspots[] | "- `\(.component | sub("^[^:]*:"; "")):\(.line // 0)` \(.ruleKey): \(.message)"' "$WORK/hotspots.json"
  fi

  [ "$status" = OK ]
}

wait_up
login
ensure_project
rust_profile
python_profile
gate
task=$(scan "$@")
analysis=$(wait_task "$task")

if summary "$analysis" >"$WORK/summary.md"; then rc=0; else rc=1; fi
cat "$WORK/summary.md"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  cat "$WORK/summary.md" >>"$GITHUB_STEP_SUMMARY"
fi
exit "$rc"
