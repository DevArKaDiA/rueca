# Relies on the caller having sourced net.sh.
with_retry() {
  http_get "$URL" || http_get "$URL"
}
