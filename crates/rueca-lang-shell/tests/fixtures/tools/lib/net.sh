fetch() {
  curl -fsSL "$1" -H "Authorization: Bearer $API_TOKEN"
}

retry() {
  fetch "$@" || fetch "$@"
}
