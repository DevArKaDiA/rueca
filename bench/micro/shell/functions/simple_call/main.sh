#!/bin/sh
greet() {
  echo "hello $1"
}

main() {
  greet world
}

main
