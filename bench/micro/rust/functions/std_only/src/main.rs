fn describe(v: &[i32]) -> String {
    let mut s = String::new();
    s.push_str(&v.len().to_string());
    std::mem::drop(v);
    s
}

fn main() {
    let v = vec![1, 2, 3];
    let d = describe(&v);
    println!("{}", d.trim());
}
