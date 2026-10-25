package com.acme.core;

import java.util.HashMap;
import java.util.Map;
import java.util.function.Function;

/** A generic registry. */
public class Registry<T extends Shape> {
    private final Map<String, T> items = new HashMap<>();

    public <R> R lookup(String key, Function<T, R> mapper) {
        T item = items.get(key);
        return mapper.apply(item);
    }

    public void register(T shape) {
        items.put(shape.name(), shape);
    }
}
