trait Shape {
    fn area(&self) -> f64;
}

struct Circle(f64);

impl Shape for Circle {
    fn area(&self) -> f64 {
        3.14 * self.0 * self.0
    }
}

fn total<T: Shape>(items: &[T]) -> f64 {
    items.iter().map(|s| s.area()).sum()
}

fn largest<T>(a: &T, b: &T) -> f64
where
    T: Shape,
{
    a.area().max(b.area())
}

fn main() {
    let v = [Circle(1.0), Circle(2.0)];
    println!("{} {}", total(&v), largest(&v[0], &v[1]));
}
