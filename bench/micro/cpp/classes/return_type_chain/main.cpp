class Leaf {
public:
    int weight() const { return 1; }
};

class Branch {
public:
    const Leaf &leaf() const { return l; }

private:
    Leaf l;
};

class Tree {
public:
    const Branch &branch() const { return b; }

private:
    Branch b;
};

int weigh(const Tree &t) { return t.branch().leaf().weight(); }
