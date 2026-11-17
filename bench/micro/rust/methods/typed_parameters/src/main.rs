struct Engine;

impl Engine {
    fn start(&self) -> u8 {
        1
    }

    fn stop(self) -> u8 {
        0
    }
}

fn by_ref(e: &Engine) -> u8 {
    e.start()
}

fn by_box(e: Box<Engine>) -> u8 {
    e.stop()
}

fn by_option(e: Option<Engine>) -> u8 {
    match e {
        Some(engine) => engine.start(),
        None => 0,
    }
}

fn by_slice(v: &[Engine]) -> u8 {
    v[0].start()
}

fn main() {
    let t = by_ref(&Engine) + by_box(Box::new(Engine)) + by_option(Some(Engine)) + by_slice(&[Engine]);
    println!("{t}");
}
