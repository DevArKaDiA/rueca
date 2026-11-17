pub fn connect(host: &str) -> bool {
    resolve(host).is_some()
}

fn resolve(host: &str) -> Option<u32> {
    if host.is_empty() {
        None
    } else {
        Some(1)
    }
}
