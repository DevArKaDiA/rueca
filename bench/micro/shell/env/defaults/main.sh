#!/bin/bash
default_port() {
  echo 8080
}

load_config() {
  echo /etc/app.conf
}

serve() {
  local port="${PORT:-$(default_port)}"
  : "${CONFIG:=$(load_config)}"
  echo "$port $CONFIG"
}
