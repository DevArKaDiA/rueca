namespace {
int secret() { return 42; }
}

namespace api {
int reveal() { return secret(); }
}
