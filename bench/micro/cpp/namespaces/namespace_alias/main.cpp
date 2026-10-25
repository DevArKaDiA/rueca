namespace lib {
namespace io {
int read() { return 0; }
int write() { return 1; }
}
}

namespace fs = lib::io;

int load() { return fs::read(); }
