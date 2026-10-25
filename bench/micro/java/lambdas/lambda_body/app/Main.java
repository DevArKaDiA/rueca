package app;

import java.util.function.Supplier;

class Metrics {
    int compute() {
        return 1;
    }

    Supplier<Integer> lazy() {
        return () -> compute();
    }
}
