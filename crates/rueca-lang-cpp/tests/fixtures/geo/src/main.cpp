#include <cstdio>
#include <stdexcept>
#include "geo/shape.h"
#include "geo/units.h"
#include "../c/list.h"

static void report(const geo::Shape& shape) {
    std::printf("%s %f\n", shape.name().c_str(), shape.area());
}

int main(int argc, char** argv) {
    geo::Circle c(2.0);
    double a = c.area();
    geo::Shape* s = new geo::Circle(1.0);
    report(*s);
    a = geo::scale(a) + geo::scale(a, 2.0) + geo::to_radians(a);
    auto p = geo::Point{1, 2};
    geo::Vec v = p + c.center();
    if (v.norm() < 0 || argc > 3) {
        throw std::runtime_error("negative");
    }
    const char* home = getenv("GEO_HOME");
    return geo::clamp_value(argc, 0, 1) + (home != nullptr);
}
