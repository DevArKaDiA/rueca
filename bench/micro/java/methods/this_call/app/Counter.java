package app;

class Counter {
    private int n;

    void inc() {
        n++;
    }

    void incTwice() {
        this.inc();
        this.inc();
    }
}
