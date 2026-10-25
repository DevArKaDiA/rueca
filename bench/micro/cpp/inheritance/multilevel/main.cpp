class A {
public:
    int root() const { return 1; }
};

class B : public A {};

class C : public B {
public:
    int leaf() const { return root(); }
};

int walk(const C &c) { return c.root() + c.leaf(); }
