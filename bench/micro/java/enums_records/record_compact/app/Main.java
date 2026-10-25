package app;

record Range(int lo, int hi) {
    Range {
        validate(lo, hi);
    }

    static void validate(int lo, int hi) {
    }

    int width() {
        return hi - lo;
    }
}

class Ranges {
    int span() {
        Range r = new Range(1, 5);
        return r.width() + r.lo();
    }
}
