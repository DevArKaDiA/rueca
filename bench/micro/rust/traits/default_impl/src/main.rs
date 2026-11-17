#[derive(Debug)]
struct Config {
    level: u8,
}

impl Default for Config {
    fn default() -> Self {
        Config { level: 3 }
    }
}

fn main() {
    let a = Config::default();
    let b: Config = Default::default();
    println!("{:?} {:?} {}", a, b, a.level);
}
