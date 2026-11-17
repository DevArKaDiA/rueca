fn helper() -> i32 {
    41
}

fn run() -> i32 {
    helper() + 1
}

fn main() {
    let x = run();
    println!("{x}");
}
