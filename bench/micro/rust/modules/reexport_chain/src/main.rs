mod engine {
    mod imp {
        pub fn work() -> i32 {
            7
        }
    }

    pub use self::imp::work;
}

mod facade {
    pub use crate::engine::work;
}

fn main() {
    let v = facade::work();
    println!("{v}");
}
