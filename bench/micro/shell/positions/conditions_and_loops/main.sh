#!/bin/bash
check() { true; }
act() { :; }
poll() { false; }
step() { :; }
on_start() { :; }

main() {
  if check; then
    act
  fi
  while poll; do
    step
  done
  case "$1" in
    start) on_start ;;
  esac
}
