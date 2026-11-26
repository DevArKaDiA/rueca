#!/bin/bash
ls() {
  command ls --color=auto "$@"
}

listing() {
  command ls -1
  ls -la
}
