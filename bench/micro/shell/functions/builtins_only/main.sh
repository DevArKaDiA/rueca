#!/bin/sh
report() {
  printf '%s\n' "$1"
  echo "done" | grep -q d
  test -n "$1" && cd /tmp
}
