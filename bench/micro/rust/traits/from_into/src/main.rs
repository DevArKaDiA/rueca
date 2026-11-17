struct Meters(u32);

impl From<u32> for Meters {
    fn from(v: u32) -> Self {
        Meters(v)
    }
}

fn main() {
    let m = Meters::from(5);
    let n: Meters = 7.into();
    println!("{} {}", m.0, n.0);
}
