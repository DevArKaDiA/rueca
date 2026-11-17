struct Point {
    x: i32,
    y: i32,
}

struct Meters(u32);

struct Unit;

enum Shape {
    Circle(f64),
    Square { side: f64 },
    Empty,
}

fn build() -> (Point, Meters, Unit, Shape) {
    let p = Point { x: 1, y: 2 };
    let m = Meters(3);
    let s = Shape::Circle(1.0);
    let _q = Shape::Square { side: 2.0 };
    let _e = Shape::Empty;
    let _o = Some(4);
    (p, m, Unit, s)
}

fn main() {
    let (p, m, _u, _s) = build();
    println!("{} {} {}", p.x, p.y, m.0);
}
