struct Plain {
    int get() const { return 5; }
};

int use_plain() {
    Plain *p = new Plain();
    int v = p->get();
    delete p;
    return v;
}
