#include <catch2/catch_test_macros.hpp>
#include "geo/shape.h"

TEST_CASE("scale doubles", "[scale]") {
    REQUIRE(geo::scale(2.0, 2.0) == 4.0);
}
