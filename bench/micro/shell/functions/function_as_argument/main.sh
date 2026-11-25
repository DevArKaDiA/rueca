#!/bin/bash
retry() {
  "$@" || "$@"
}

fetch_page() {
  curl -fsS "$1"
}

main() {
  retry fetch_page https://example.com
}
