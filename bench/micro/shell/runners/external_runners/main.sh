#!/bin/bash
helper() {
  echo "$1"
}

main() {
  xargs -n1 helper < list.txt
  sudo helper
  env A=1 helper
  nohup helper &
}
