fn helper() -> i32 {
    1
}

fn main() {
    let v = helper();
    println!("{v}");
}

#[test]
fn top_level_test() {
    assert_eq!(helper(), 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> i32 {
        helper() + 1
    }

    #[test]
    fn it_works() {
        assert_eq!(fixture(), 2);
    }
}
