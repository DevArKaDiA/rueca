#ifndef GEO_SHAPE_H
#define GEO_SHAPE_H

#include <string>
#include "geo/point.h"

/// Pi, good enough for geometry.
#define GEO_PI 3.14159
#define GEO_SQ(x) ((x) * (x))

namespace geo {

/// Base of every shape.
class Shape {
public:
    virtual ~Shape();
    /// Area in square units.
    virtual double area() const = 0;
    std::string name() const;

protected:
    int id_ = 0;
};

class Circle : public Shape {
public:
    explicit Circle(double r);
    double area() const override;
    double radius() const;
    double reach() const;
    Point center() const { return center_; }

private:
    double r_;
    Point center_;
};

double scale(double v);
double scale(double v, double factor);

template <typename T>
T clamp_value(T v, T lo, T hi) {
    return v < lo ? lo : (v > hi ? hi : v);
}

enum class Color { Red, Green };

namespace detail {
int next_id();
}  // namespace detail

}  // namespace geo

#endif  // GEO_SHAPE_H
