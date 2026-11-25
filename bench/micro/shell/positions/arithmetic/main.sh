#!/bin/bash
count() {
  echo 3
}

next() {
  echo $(( $(count) + 1 ))
}
