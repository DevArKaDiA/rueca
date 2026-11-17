struct Wrapper(i32);

fn wrapper(x: i32) -> Wrapper {
    Wrapper(x)
}

fn main() {
    let a = wrapper(1);
    let b = Wrapper(2);
    println!("{} {}", a.0, b.0);
}
