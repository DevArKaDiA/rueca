template <typename F>
int each(F f) {
    return f(1) + f(2);
}

int note(int v) { return v; }

int over_all() {
    return each([](int v) { return note(v); });
}
