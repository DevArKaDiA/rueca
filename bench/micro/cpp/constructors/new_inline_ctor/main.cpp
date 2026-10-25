class Widget {
public:
    Widget(int n) : n(n) {}
    int size() const { return n; }

private:
    int n;
};

int make() {
    Widget *w = new Widget(3);
    int s = w->size();
    delete w;
    return s;
}
