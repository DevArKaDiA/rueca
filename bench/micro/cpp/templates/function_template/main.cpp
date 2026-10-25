template <typename T>
T max_of(T a, T b) {
    return a > b ? a : b;
}

int ints() { return max_of(1, 2); }
double doubles() { return max_of<double>(1.0, 2.0); }
