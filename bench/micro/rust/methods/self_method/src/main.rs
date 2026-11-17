struct Counter {
    n: i32,
}

impl Counter {
    fn inc(&mut self) {
        self.add(1);
    }

    fn add(&mut self, k: i32) {
        self.n += k;
    }
}

fn main() {
    let mut c = Counter { n: 0 };
    c.inc();
    println!("{}", c.n);
}
