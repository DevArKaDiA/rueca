package app;

class Shape {
    double area() {
        return 0;
    }
}

class Circle extends Shape {
    @Override
    double area() {
        return 3.14;
    }
}

class Canvas {
    double measure() {
        Shape s = new Circle();
        return s.area();
    }
}
