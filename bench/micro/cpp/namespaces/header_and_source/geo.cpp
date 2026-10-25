#include "geo.h"

namespace geo {
double area(double w, double h) { return w * h; }
}

double geo::perimeter(double w, double h) { return 2 * (w + h); }
