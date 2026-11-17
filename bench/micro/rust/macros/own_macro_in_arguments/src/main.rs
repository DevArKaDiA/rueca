macro_rules! double {
    ($x:expr) => {
        $x * 2
    };
}

fn base() -> i32 {
    21
}

fn main() {
    println!("{}", double!(base()));
}
