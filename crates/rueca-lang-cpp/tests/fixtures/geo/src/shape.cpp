#include "geo/shape.h"

namespace {
std::string describe(int id) { return std::to_string(id); }
}  // namespace

namespace geo {

Shape::~Shape() {}

std::string Shape::name() const { return describe(id_); }

Circle::Circle(double r) : r_(r) { id_ = detail::next_id(); }

double Circle::area() const { return GEO_PI * GEO_SQ(r_); }

double scale(double v) { return v; }

double scale(double v, double factor) { return v * factor; }

namespace detail {
int next_id() {
    static int counter = 0;
    return ++counter;
}
}  // namespace detail

}  // namespace geo

double geo::Circle::radius() const { return r_; }

double geo::Point::norm() const { return x * x + y * y; }

geo::Point geo::operator+(const Point& a, const Point& b) { return Point{a.x + b.x, a.y + b.y}; }
