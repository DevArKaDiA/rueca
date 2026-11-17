use shop_core::store::Store;
use shop_core::Item;

/// Runs the command line.
pub fn run(store: &mut dyn Store) {
    let mode = std::env::var("SHOP_MODE").unwrap_or_default();
    if store.find(&mode).is_none() {
        store.save_all(vec![Item::new(&mode, 1)]);
    }
    report(super::compute(shop_core::Price::new(4)));
}

pub fn report(n: u64) {
    println!("{n} {}", env!("CARGO_PKG_NAME"));
}
