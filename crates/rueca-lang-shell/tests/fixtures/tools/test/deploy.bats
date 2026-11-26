load helpers

@test "log writes to stderr" {
  run log hello
  [ "$status" -eq 0 ]
}
