trait Named {
    fn name(&self) -> String;
}

trait Greeter: Named {
    fn greet(&self) -> String {
        format!("hello {}", self.name())
    }
}

struct Bob;

impl Named for Bob {
    fn name(&self) -> String {
        "bob".to_string()
    }
}

impl Greeter for Bob {}

fn main() {
    println!("{}", Bob.greet());
}
