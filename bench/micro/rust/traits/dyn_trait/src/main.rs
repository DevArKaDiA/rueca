trait Shape {
    fn area(&self) -> f64;
}

struct Square(f64);

impl Shape for Square {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}

fn area_of(s: &dyn Shape) -> f64 {
    s.area()
}

fn sum(shapes: Vec<Box<dyn Shape>>) -> f64 {
    shapes.iter().map(|s| s.area()).sum()
}

fn main() {
    let total = area_of(&Square(1.0)) + sum(vec![Box::new(Square(2.0))]);
    println!("{total}");
}
