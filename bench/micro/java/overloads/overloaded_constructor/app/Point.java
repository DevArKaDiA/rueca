package app;

class Point {
    final int x;
    final int y;

    Point() {
        this(0, 0);
    }

    Point(int x, int y) {
        this.x = x;
        this.y = y;
    }
}

class Shapes {
    Point origin() {
        return new Point();
    }

    Point at(int x, int y) {
        return new Point(x, y);
    }
}
