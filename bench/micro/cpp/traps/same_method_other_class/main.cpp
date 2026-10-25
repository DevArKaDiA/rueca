class List {
public:
    int size() const { return 1; }
};

class Map {
public:
    int size() const { return 2; }
};

int list_size(const List &l) { return l.size(); }
