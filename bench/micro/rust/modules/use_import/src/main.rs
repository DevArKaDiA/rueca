mod util {
    pub fn helper() -> i32 {
        1
    }
}

use util::helper;

fn main() {
    let v = helper();
    println!("{v}");
}
