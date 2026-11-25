#!/bin/bash
left() {
  echo a
}

right() {
  echo b
}

compare() {
  diff <(left) <(right)
}
