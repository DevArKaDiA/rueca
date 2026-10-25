#include <gtest/gtest.h>
#include "geo/shape.h"

class CircleFixture : public ::testing::Test {
protected:
    geo::Circle circle{2.0};
    void reset();
};

void CircleFixture::reset() {}

TEST(CircleTest, Area) {
    geo::Circle c(1.0);
    EXPECT_NEAR(c.area(), 3.14, 0.01);
}

TEST_F(CircleFixture, Radius) {
    EXPECT_EQ(circle.radius(), 2.0);
    reset();
}
