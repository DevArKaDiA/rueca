#!/bin/bash
failing() {
  return 1
}

slow() {
  sleep 2
}

main() {
  ! failing
  time slow
}
