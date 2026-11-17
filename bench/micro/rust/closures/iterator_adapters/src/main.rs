fn double(x: i32) -> i32 {
    x * 2
}

fn is_big(x: &i32) -> bool {
    *x > 2
}

fn main() {
    let v: Vec<i32> = (1..5).map(|x| double(x)).filter(|x| is_big(x)).collect();
    let s: i32 = v.iter().fold(0, |acc, x| acc + double(*x));
    println!("{s}");
}
