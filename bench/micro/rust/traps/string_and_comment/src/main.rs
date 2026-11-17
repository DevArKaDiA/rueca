fn helper() -> &'static str {
    "helper()"
}

fn main() {
    // helper() is called below only once
    /* helper(); */
    let s = "helper();";
    let r = helper();
    println!("{s} {r}");
}
