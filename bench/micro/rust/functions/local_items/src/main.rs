fn outer() -> i32 {
    struct Local;

    impl Local {
        fn value(&self) -> i32 {
            5
        }
    }

    Local.value()
}

fn main() {
    let v = outer();
    println!("{v}");
}
