fn inc(x: i32) -> i32 {
    x + 1
}

fn make_adder() -> impl Fn(i32) -> i32 {
    move |x| inc(x)
}

fn boxed() -> Box<dyn Fn(i32) -> i32> {
    Box::new(|x| inc(inc(x)))
}

fn main() {
    let f = make_adder();
    let g = boxed();
    println!("{} {}", f(1), g(2));
}
