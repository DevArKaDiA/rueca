#!/bin/bash
countdown() {
  local n=$1
  if [ "$n" -gt 0 ]; then
    countdown $((n - 1))
  fi
}
