package app;

class MathOps {
    static long fact(int n) {
        return n <= 1 ? 1 : n * fact(n - 1);
    }

    boolean isEven(int n) {
        return n == 0 || isOdd(n - 1);
    }

    boolean isOdd(int n) {
        return n != 0 && isEven(n - 1);
    }
}
