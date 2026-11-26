#!/bin/bash
run_job() {
  echo "debug=$DEBUG"
}

compute() {
  echo 7
}

main() {
  DEBUG=1 run_job
  export RESULT=$(compute)
}
