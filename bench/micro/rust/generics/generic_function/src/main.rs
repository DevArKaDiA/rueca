fn largest<T: PartialOrd + Copy>(v: &[T]) -> T {
    let mut m = v[0];
    for &x in v {
        if x > m {
            m = x;
        }
    }
    m
}

fn pair<A, B>(a: A, b: B) -> (A, B) {
    (a, b)
}

fn main() {
    let a = largest(&[1, 5, 3]);
    let b = largest::<f64>(&[1.0, 2.0]);
    let p = pair(a, b);
    println!("{p:?}");
}
