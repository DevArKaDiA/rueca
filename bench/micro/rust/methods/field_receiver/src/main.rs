struct Engine;

impl Engine {
    fn start(&self) -> bool {
        true
    }
}

struct Car {
    engine: Engine,
}

impl Car {
    fn drive(&self) -> bool {
        self.engine.start()
    }
}

fn main() {
    let car = Car { engine: Engine };
    let ok = car.drive() && car.engine.start();
    println!("{ok}");
}
