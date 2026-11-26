# Shared helpers, sourced by every script.
APP_VERSION=1.4.2
source "$(dirname "${BASH_SOURCE[0]}")/net.sh"

log() {
  printf '%s %s\n' "$(date +%T)" "$*" >&2
}
