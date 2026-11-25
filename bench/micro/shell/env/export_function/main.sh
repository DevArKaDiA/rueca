#!/bin/bash
helper() {
  echo "$HOME"
}
export -f helper
export PATH="$PATH:/opt/bin"

main() {
  helper
}
