class Parser {
public:
    int parse(int v) { return v; }
    int parse(double v) { return static_cast<int>(v); }
};

int parse_all(Parser &p) { return p.parse(1) + p.parse(2.5); }
