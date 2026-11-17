mod cli;

use shop_core::{Item, Price, Store, traced};

fn make() -> Item {
    Item::new("book", 1500)
}

fn main() {
    let mut store = shop_core::open_store();
    let item = make();
    let total = traced!(item.total(2));
    println!("{} {}", total, compute(item.discounted()));
    store.save(item);
    let double = |p: Price| p.cents() * 2;
    let show = |x: u64| cli::report(x);
    show(double(Price::new(1)));
    cli::run(&mut store);
    let raw = serde_json::to_string(&total);
    let _ = raw;
    let _ = reopen();
}

fn reopen() -> std::io::Result<usize> {
    let store = shop_core::MemoryStore::open("shop.db")?;
    Ok(store.len())
}

fn compute(p: Price) -> u64 {
    fn half(v: u64) -> u64 {
        v / 2
    }
    half(p.cents())
}
