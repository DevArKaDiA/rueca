mod shapes {
    pub fn circle() -> f64 {
        3.14
    }

    pub fn square() -> f64 {
        1.0
    }
}

use shapes::*;

fn main() {
    let t = circle() + square();
    println!("{t}");
}
