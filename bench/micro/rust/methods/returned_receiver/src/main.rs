struct Config {
    debug: bool,
}

impl Config {
    fn is_debug(&self) -> bool {
        self.debug
    }
}

fn load() -> Config {
    Config { debug: true }
}

fn main() {
    let d = load().is_debug();
    println!("{d}");
}
