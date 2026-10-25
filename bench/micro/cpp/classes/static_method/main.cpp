class Conn {
public:
    static Conn *open() { return nullptr; }
    void close() {}
};

void cycle() {
    Conn *c = Conn::open();
    c->close();
}

void cycle_chained() { Conn::open()->close(); }
