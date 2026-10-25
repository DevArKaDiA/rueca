class Writer {
public:
    void write(int) {}
    void write(int, int) {}
    void write(const char *) {}
};

void save(Writer &w) { w.write(1, 2); }
