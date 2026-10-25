package com.acme.core;

public abstract class Base implements AutoCloseable {
    protected int runs;

    public void run() {
        runs++;
        log("run");
    }

    protected void log(String message) {}

    @Override
    public void close() {}
}
