use shop_core::{Item, Priced, Store};

#[test]
fn item_label() {
    let item = Item::new("cup", 300);
    assert_eq!(item.label(), "300 cents");
    let mut store = shop_core::open_store();
    store.save(item);
}
