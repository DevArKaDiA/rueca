#!/bin/bash
setup_env() {
  export READY=1
}

main() {
  eval "setup_env"
  eval "$DYNAMIC_COMMAND"
}
