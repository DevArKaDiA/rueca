#!/bin/bash
setup() {
  mkdir -p /tmp/x
}

worker() {
  sleep 1
}

main() {
  ( setup )
  worker &
  wait
}
