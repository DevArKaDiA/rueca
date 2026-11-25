#!/bin/bash
deploy=yes

deploy() {
  echo "deploy=$deploy"
}

run() {
  local deploy=no
  deploy
  echo "$deploy"
}
