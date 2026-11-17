mod geometry {
    pub struct Square(pub f64);

    impl Square {
        pub fn area(&self) -> f64 {
            self.0 * self.0
        }

        pub fn double(&self) -> f64 {
            self.area() * 2.0
        }
    }

    pub trait Scale {
        fn scale(&self) -> f64;
    }

    impl Scale for Square {
        fn scale(&self) -> f64 {
            self.double()
        }
    }
}

use geometry::{Scale, Square};

fn main() {
    let s = Square(2.0);
    println!("{} {}", s.area(), s.scale());
}
