class Counter {
public:
    void inc() { ++n; }
    int value() const { return n; }

private:
    int n = 0;
};

int tally(Counter &c) {
    c.inc();
    return c.value();
}
