#!/bin/bash
greet() {
  echo hi
}

main() {
  local cmd=greet
  $cmd
  "${cmd}"
}
