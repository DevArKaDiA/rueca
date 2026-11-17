fn double(x: i32) -> i32 {
    x * 2
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    let r = add(double(1), double(add(2, 3)));
    println!("{r}");
}
