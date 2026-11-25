#!/bin/bash
# Portable wrappers named like the utilities they replace.
realpath() {
  python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

log() {
  echo "[log] $*" >&2
}

resolve() {
  log "resolving $1"
  realpath "$1"
}
