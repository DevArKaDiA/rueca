struct Item {
    id: u32,
}

impl Item {
    fn label(&self) -> String {
        format!("item-{}", self.id)
    }
}

fn compute(x: u32) -> u32 {
    x + 1
}

fn make(id: u32) -> Item {
    Item { id }
}

fn main() {
    let items = vec![make(1), make(compute(2))];
    println!("{}", items[0].label());
    assert_eq!(compute(1), 2);
    let s = format!("{}-{}", compute(3), items[1].label());
    debug_assert!(!s.is_empty());
}
