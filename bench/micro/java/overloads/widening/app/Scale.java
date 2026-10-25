package app;

class Scale {
    long twice(long x) {
        return 2 * x;
    }

    double twice(double x) {
        return 2 * x;
    }

    long apply(int n) {
        return twice(n);
    }
}
