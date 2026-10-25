package app;

abstract class Figure {
    abstract double area();
}

class Square extends Figure {
    @Override
    double area() {
        return 4;
    }
}

class Stats {
    <F extends Figure> double sum(F a, F b) {
        return a.area() + b.area();
    }
}
