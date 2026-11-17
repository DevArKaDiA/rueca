#[macro_export]
macro_rules! ensure {
    ($c:expr) => {
        if !$c {
            panic!("failed");
        }
    };
}

fn check(v: i32) -> i32 {
    crate::ensure!(v > 0);
    v
}

fn main() {
    let v = check(1);
    println!("{v}");
}
