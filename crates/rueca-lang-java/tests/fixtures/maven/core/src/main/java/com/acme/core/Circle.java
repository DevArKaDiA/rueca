package com.acme.core;

/** A circle; record components are fields. */
public record Circle(double radius, String label) implements Shape {
    public Circle {
        if (radius < 0) throw new IllegalArgumentException("radius");
    }

    public Circle(double radius) {
        this(radius, "circle");
    }

    @Override
    public double area() {
        return Math.PI * radius * radius;
    }

    @Override
    public String name() {
        return label;
    }
}
