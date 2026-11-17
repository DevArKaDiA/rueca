fn compute(x: i32) -> i32 {
    x * 10
}

fn main() {
    let compute = |x: i32| x + 1;
    let r = compute(2);
    println!("{r}");
}
