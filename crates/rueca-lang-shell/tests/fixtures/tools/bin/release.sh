#!/bin/bash
# Releases to production through the deploy script.
release() {
  bash "$(dirname "$0")/deploy" production
}
release
