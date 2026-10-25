class Point {
public:
    Point(int x) : x(x), y(0) {}
    Point(int x, int y) : x(x), y(y) {}

private:
    int x;
    int y;
};

Point *on_axis() { return new Point(1); }
Point *anywhere() { return new Point(1, 2); }
