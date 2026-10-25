package app;

class Sum {
    static int total(int... xs) {
        int t = 0;
        for (int x : xs) {
            t += x;
        }
        return t;
    }

    static int total(int a, int b) {
        return a + b;
    }

    int three() {
        return total(1, 2, 3);
    }

    int two() {
        return total(1, 2);
    }
}
