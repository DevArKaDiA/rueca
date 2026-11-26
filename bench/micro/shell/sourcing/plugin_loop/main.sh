#!/bin/bash
for plugin in plugins/*.sh; do
  source "$plugin"
done

run() {
  plugin_hello
}
