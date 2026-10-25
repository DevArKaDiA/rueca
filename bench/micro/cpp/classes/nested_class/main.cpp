class Outer {
public:
    class Inner {
    public:
        static int helper() { return 1; }
        int value() const;
    };
    int run(const Inner &i) { return Inner::helper() + i.value(); }
};

int Outer::Inner::value() const { return 2; }
