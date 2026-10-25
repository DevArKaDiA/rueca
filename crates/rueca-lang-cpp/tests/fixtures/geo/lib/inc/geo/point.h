#pragma once

namespace geo {

struct Point {
    double x = 0;
    double y = 0;
    double norm() const;
};

// Component-wise sum.
Point operator+(const Point& a, const Point& b);

using Vec = Point;

}  // namespace geo
