int run() { return 0; }

namespace a {
int run() { return 1; }
int start() { return run(); }
}

namespace b {
int run() { return 2; }
}

int global_start() { return run(); }
