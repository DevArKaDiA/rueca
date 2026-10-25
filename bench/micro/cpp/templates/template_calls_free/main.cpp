int validate(int v) { return v; }

template <typename T>
T checked(T v) {
    validate(1);
    return v;
}

long use_checked() { return checked<long>(3L); }
