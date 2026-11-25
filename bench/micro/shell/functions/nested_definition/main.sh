#!/bin/bash
outer() {
  inner() {
    helper
  }
  inner
}

helper() {
  echo helping
}
