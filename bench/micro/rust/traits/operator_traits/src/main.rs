use std::ops::Add;

#[derive(Clone, Copy)]
struct V(i32);

impl Add for V {
    type Output = V;

    fn add(self, o: V) -> V {
        V(self.0 + o.0)
    }
}

fn sum_operator(a: V, b: V) -> V {
    a + b
}

fn sum_explicit(a: V, b: V) -> V {
    a.add(b)
}

fn main() {
    let r = sum_operator(V(1), V(2));
    let s = sum_explicit(V(1), V(2));
    println!("{} {}", r.0, s.0);
}
