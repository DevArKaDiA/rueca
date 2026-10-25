class Base {
public:
    int id() const { return 1; }
};

class Derived : public Base {
public:
    int extra() const { return 2; }
};

int describe(const Derived &d) { return d.id() + d.extra(); }
