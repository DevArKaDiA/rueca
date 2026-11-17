fn double(x: i32) -> i32 {
    x * 2
}

fn apply(f: fn(i32) -> i32, x: i32) -> i32 {
    f(x)
}

fn main() {
    let g = double;
    let a = g(1);
    let b = apply(double, 2);
    let c: Vec<i32> = [1, 2].iter().copied().map(double).collect();
    println!("{a} {b} {c:?}");
}
