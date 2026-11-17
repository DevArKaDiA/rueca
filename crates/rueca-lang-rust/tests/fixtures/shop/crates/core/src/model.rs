use std::fmt;

use crate::Priced;

/// An item for sale.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub price: Price,
}

/// A price in cents.
#[derive(Debug, Clone, Copy)]
pub struct Price(u64);

impl Price {
    pub fn new(cents: u64) -> Self {
        Price(cents)
    }

    pub fn cents(&self) -> u64 {
        self.0
    }
}

impl Item {
    pub fn new(name: &str, cents: u64) -> Item {
        Item { name: name.to_string(), price: Price::new(cents) }
    }

    pub fn total(&self, qty: u64) -> u64 {
        self.price.cents() * qty
    }

    pub fn discounted(&self) -> Price {
        Price::new(self.total(1) / 2)
    }
}

impl Priced for Item {
    fn price(&self) -> Price {
        self.price
    }
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.label())
    }
}
