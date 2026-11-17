struct Builder {
    name: String,
    size: u32,
}

struct Widget {
    label: String,
}

impl Builder {
    fn new() -> Builder {
        Builder { name: String::new(), size: 0 }
    }

    fn name(mut self, n: &str) -> Builder {
        self.name = n.to_string();
        self
    }

    fn size(mut self, s: u32) -> Builder {
        self.size = s;
        self
    }

    fn build(self) -> Widget {
        Widget { label: format!("{}:{}", self.name, self.size) }
    }
}

fn main() {
    let w = Builder::new().name("w").size(3).build();
    println!("{}", w.label);
}
