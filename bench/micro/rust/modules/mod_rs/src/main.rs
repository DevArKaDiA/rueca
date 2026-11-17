mod store;

fn main() {
    let d = store::open();
    println!("{d}");
}
