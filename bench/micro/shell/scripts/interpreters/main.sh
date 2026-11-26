#!/bin/bash
build_all() {
  bash scripts/build.sh --release
  sh -c 'build'
  zsh tools/check.zsh
}
