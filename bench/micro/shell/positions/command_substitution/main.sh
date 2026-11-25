#!/bin/bash
get_value() {
  echo 42
}

fmt() {
  printf '<%s>' "$1"
}

stamp() {
  date +%s
}

main() {
  local x
  x=$(get_value)
  local y="$(fmt "$x")"
  local z=`stamp`
  echo "$y$z"
}
