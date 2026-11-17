use std::collections::HashMap;
use std::sync::Arc;

use super::Store;
use crate::model::Item;
use crate::util;

/// A cache of lookups.
pub struct Cache {
    hits: u64,
}

impl Cache {
    pub fn record(&self) -> u64 {
        self.hits + 1
    }
}

/// A store in memory.
pub struct MemoryStore {
    items: HashMap<String, Item>,
    cache: Arc<Cache>,
}

impl MemoryStore {
    pub fn new() -> Self {
        MemoryStore { items: HashMap::new(), cache: Arc::new(Cache { hits: 0 }) }
    }

    /// Opens a store kept at `path`.
    pub fn open(path: &str) -> std::io::Result<Self> {
        util::trace(path);
        Ok(Self::new())
    }

    pub fn len(&self) -> usize {
        self.cache.record();
        self.items.len()
    }
}

impl Store for MemoryStore {
    fn save(&mut self, item: Item) {
        util::trace("save");
        self.items.insert(item.name.clone(), item);
    }

    fn find(&self, name: &str) -> Option<&Item> {
        let found = self.items.get(name);
        crate::util::trace(name);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Item {
        Item::new("pen", 120)
    }

    #[test]
    fn saves() {
        let mut s = MemoryStore::new();
        s.save(sample());
        assert_eq!(s.len(), 1);
    }
}
