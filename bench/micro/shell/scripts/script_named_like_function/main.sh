#!/bin/bash
build() {
  echo building
}

run() {
  build.sh
  "$(dirname "$0")/build"
  build
}
