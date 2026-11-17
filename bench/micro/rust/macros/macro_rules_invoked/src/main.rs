fn mul(a: i32, b: i32) -> i32 {
    a * b
}

macro_rules! square {
    ($x:expr) => {
        mul($x, $x)
    };
}

fn main() {
    let v = square!(3);
    println!("{v}");
}
