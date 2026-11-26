greet() {
  print -P "%F{green}hi%f"
}

function main {
  local -a names
  names=(a b c)
  greet
}
