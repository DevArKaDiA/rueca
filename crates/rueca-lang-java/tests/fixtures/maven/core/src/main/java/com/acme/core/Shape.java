package com.acme.core;

/** A shape with an area. */
public interface Shape {
    double area();

    /** Default methods are inherited by implementations. */
    default String describe() {
        return name() + " with area " + area();
    }

    String name();
}
