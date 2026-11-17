enum Shape {
    Circle(f64),
    Rect(f64, f64),
}

fn circle_area(r: f64) -> f64 {
    3.14 * r * r
}

fn rect_area(w: f64, h: f64) -> f64 {
    w * h
}

impl Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(r) => circle_area(*r),
            Shape::Rect(w, h) => rect_area(*w, *h),
        }
    }

    fn unit() -> Shape {
        Shape::Circle(1.0)
    }
}

fn main() {
    let a = Shape::unit().area() + Shape::Rect(1.0, 2.0).area();
    println!("{a}");
}
