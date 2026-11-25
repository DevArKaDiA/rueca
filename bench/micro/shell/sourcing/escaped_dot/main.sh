#!/bin/bash
DIR="$(dirname "$0")"
[ -s "$DIR/lib.sh" ] && echo found
\. "$DIR/lib.sh"

main() {
  lib_func
}
