namespace a {
int run() { return 1; }
}

namespace b {
int run() { return 2; }
}

int pick() { return a::run(); }
