trait Speak {
    fn name(&self) -> String;

    fn describe(&self) -> String {
        format!("I am {}", self.name())
    }
}

struct Dog;

impl Speak for Dog {
    fn name(&self) -> String {
        "dog".to_string()
    }
}

fn main() {
    let d = Dog;
    println!("{}", d.describe());
}
