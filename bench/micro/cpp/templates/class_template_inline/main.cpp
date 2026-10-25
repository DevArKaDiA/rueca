template <typename T>
class Box {
public:
    T get() const { return v; }
    void set(T x) { v = x; }

private:
    T v{};
};

int roundtrip(Box<int> &b) {
    b.set(4);
    return b.get();
}
