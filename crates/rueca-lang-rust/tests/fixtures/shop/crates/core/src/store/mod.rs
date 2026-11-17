pub mod memory;

pub use self::memory::MemoryStore;

use crate::model::Item;

/// Where items are kept.
pub trait Store {
    fn save(&mut self, item: Item);
    fn find(&self, name: &str) -> Option<&Item>;

    /// Saves every item.
    fn save_all(&mut self, items: Vec<Item>) {
        for item in items {
            self.save(item);
        }
    }
}
