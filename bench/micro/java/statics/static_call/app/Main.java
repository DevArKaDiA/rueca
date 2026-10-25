package app;

class Clock {
    static long now() {
        return 0L;
    }
}

class Timer {
    long elapsed(long start) {
        return Clock.now() - start;
    }
}
