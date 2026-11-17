//! The shop's core: items, prices and stores.

pub mod model;
pub mod store;
pub mod util;

pub use model::{Item, Price};
pub use store::*;

/// Logs a call and evaluates to its value.
#[macro_export]
macro_rules! traced {
    ($e:expr) => {{
        $crate::util::trace(stringify!($e));
        $e
    }};
}

/// Something with a price.
pub trait Priced {
    fn price(&self) -> Price;

    /// The price as text.
    fn label(&self) -> String {
        format!("{} cents", self.price().cents())
    }
}

/// Builds the default store.
pub fn open_store() -> MemoryStore {
    let store = MemoryStore::new();
    util::check_env();
    store
}
