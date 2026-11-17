fn top() -> i32 {
    1
}

mod outer {
    pub fn via_super() -> i32 {
        super::top()
    }

    pub mod inner {
        pub fn via_crate() -> i32 {
            crate::top() + super::via_super()
        }
    }
}

fn main() {
    let v = outer::inner::via_crate();
    println!("{v}");
}
