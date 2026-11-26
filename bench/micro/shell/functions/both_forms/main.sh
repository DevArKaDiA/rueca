#!/bin/bash
function first {
  second
}

second() {
  third
}

function third() {
  echo done
}
