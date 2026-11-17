trait Store<T> {
    fn put(&mut self, t: T);
}

struct MemStore {
    items: Vec<i32>,
}

impl Store<i32> for MemStore {
    fn put(&mut self, t: i32) {
        self.items.push(t);
    }
}

fn fill<S: Store<i32>>(s: &mut S) {
    s.put(1);
}

fn main() {
    let mut m = MemStore { items: Vec::new() };
    m.put(2);
    fill(&mut m);
    println!("{}", m.items.len());
}
