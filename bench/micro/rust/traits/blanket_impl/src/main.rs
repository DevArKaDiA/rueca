use std::fmt::Display;

trait Shout {
    fn shout(&self) -> String;
}

impl<T: Display> Shout for T {
    fn shout(&self) -> String {
        format!("{}!", self).to_uppercase()
    }
}

fn main() {
    println!("{}", 5.shout());
    println!("{}", "hey".shout());
}
