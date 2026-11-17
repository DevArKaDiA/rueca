mod util {
    pub fn compute() -> i32 {
        3
    }
}

use util::compute as calc;

fn main() {
    let v = calc();
    println!("{v}");
}
