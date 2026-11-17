fn helper() -> i32 {
    1
}

fn outer() -> i32 {
    fn inner() -> i32 {
        helper() * 2
    }
    inner() + inner()
}

fn main() {
    let v = outer();
    println!("{v}");
}
