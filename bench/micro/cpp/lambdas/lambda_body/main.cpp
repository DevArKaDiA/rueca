int trace() { return 1; }

int compute() {
    auto f = [] { return trace(); };
    return f();
}
