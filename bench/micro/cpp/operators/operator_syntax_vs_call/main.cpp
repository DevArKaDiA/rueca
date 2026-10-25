struct Vec {
    int x;
    Vec operator+(const Vec &o) const { return Vec{x + o.x}; }
};

Vec implicit_add(const Vec &a, const Vec &b) { return a + b; }

Vec explicit_add(const Vec &a, const Vec &b) { return a.operator+(b); }
