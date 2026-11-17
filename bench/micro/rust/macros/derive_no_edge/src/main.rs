#[derive(Debug, Clone, PartialEq, Default)]
struct Settings {
    verbose: bool,
}

fn check(a: &Settings) -> bool {
    let b = a.clone();
    b == *a
}

fn main() {
    let s = Settings::default();
    println!("{:?} {} {}", s, check(&s), s.verbose);
}
