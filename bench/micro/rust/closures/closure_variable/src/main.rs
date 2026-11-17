fn helper(x: i32) -> i32 {
    x + 1
}

fn main() {
    let add = |a: i32, b: i32| helper(a) + b;
    let r = add(1, 2);
    println!("{r}");
}
