struct Cat;
struct Dog;

impl Cat {
    fn speak(&self) -> &'static str {
        "meow"
    }
}

impl Dog {
    fn speak(&self) -> &'static str {
        "woof"
    }
}

fn main() {
    let c = Cat;
    let d = Dog;
    println!("{} {}", c.speak(), d.speak());
}
