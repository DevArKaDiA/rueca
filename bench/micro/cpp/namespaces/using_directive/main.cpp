namespace util {
int helper() { return 7; }
}

namespace other {
int helper2() { return 8; }
}

using namespace util;

int caller() { return helper(); }
