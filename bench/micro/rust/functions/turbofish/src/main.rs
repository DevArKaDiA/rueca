fn parse_num<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.parse::<T>().ok()
}

fn first<T: Clone>(v: &[T]) -> T {
    v[0].clone()
}

fn main() {
    let n = parse_num::<u8>("7");
    let f = first::<i32>(&[1, 2]);
    let g: Option<u16> = parse_num("8");
    println!("{n:?} {f} {g:?}");
}
