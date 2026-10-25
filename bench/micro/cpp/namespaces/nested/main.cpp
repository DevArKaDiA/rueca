namespace outer {
namespace inner {
int deep() { return 3; }
}
int shallow() { return inner::deep(); }
}

namespace x::y {
int modern() { return outer::inner::deep(); }
}

int top() { return outer::shallow() + x::y::modern(); }
