struct Bag {
    items: Vec<u8>,
}

impl Bag {
    fn len(&self) -> usize {
        self.items.len()
    }
}

fn len(v: &[u8]) -> usize {
    v.len()
}

fn main() {
    let b = Bag { items: vec![1, 2] };
    let n = b.len() + len(&b.items) + b.items.len();
    println!("{n}");
}
