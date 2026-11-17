mod util {
    pub fn helper() -> i32 {
        1
    }

    pub fn twice() -> i32 {
        helper() + helper()
    }
}

fn main() {
    let v = util::twice();
    println!("{v}");
}
