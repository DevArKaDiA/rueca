use std::env;

pub fn trace(what: &str) {
    if env::var("SHOP_TRACE").is_ok() {
        eprintln!("trace: {what}");
    }
}

pub(crate) fn check_env() -> bool {
    std::env::var("SHOP_DB").is_ok() && option_env!("SHOP_BUILD").is_some()
}
