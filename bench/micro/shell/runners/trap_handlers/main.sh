#!/bin/bash
cleanup() {
  rm -f "$TMPFILE"
}

on_interrupt() {
  echo interrupted
}

work() {
  trap cleanup EXIT
  trap 'on_interrupt; exit 130' INT
  sleep 10
}
