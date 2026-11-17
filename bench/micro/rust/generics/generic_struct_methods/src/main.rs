struct Stack<T> {
    items: Vec<T>,
}

impl<T: Clone> Stack<T> {
    fn new() -> Self {
        Stack { items: Vec::new() }
    }

    fn push(&mut self, t: T) {
        self.items.push(t);
    }

    fn peek(&self) -> Option<T> {
        self.items.last().cloned()
    }
}

fn main() {
    let mut s = Stack::<i32>::new();
    s.push(1);
    let mut t: Stack<String> = Stack::new();
    t.push("a".to_string());
    println!("{:?} {:?}", s.peek(), t.peek());
}
