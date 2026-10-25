class Shape {
public:
    virtual double area() const = 0;
    virtual ~Shape() {}
};

class Circle : public Shape {
public:
    double area() const override { return 3.14; }
};

double measure(const Shape &s) { return s.area(); }
