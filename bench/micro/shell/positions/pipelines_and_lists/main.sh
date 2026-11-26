#!/bin/bash
produce() { seq 3; }
consume() { cat; }
ok() { echo ok; }
fail() { echo fail; }

main() {
  produce | consume && ok || fail
}
