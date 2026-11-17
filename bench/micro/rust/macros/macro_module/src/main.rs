#[macro_use]
mod macros;

fn work() -> u32 {
    log_value!(7)
}

fn main() {
    let v = work();
    println!("{v}");
}
