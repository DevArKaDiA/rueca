struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    fn origin() -> Self {
        Self::new(0, 0)
    }

    fn shifted(&self) -> Point {
        Point::new(self.x + 1, self.y + 1)
    }
}

fn main() {
    let p = Point::origin().shifted();
    println!("{} {}", p.x, p.y);
}
