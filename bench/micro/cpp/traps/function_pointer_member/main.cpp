int square(int v) { return v * v; }

struct Handler {
    int (*fn)(int);
    int call(int v) const { return fn(v); }
};

int install() {
    Handler h{square};
    return h.call(3);
}
