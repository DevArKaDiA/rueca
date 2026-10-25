package app;

enum Op {
    PLUS {
        @Override
        int apply(int x, int y) {
            return x + y;
        }
    },
    TIMES {
        @Override
        int apply(int p, int q) {
            return p * q;
        }
    };

    abstract int apply(int a, int b);
}

class Calc {
    int run(Op op) {
        return op.apply(2, 3);
    }
}
