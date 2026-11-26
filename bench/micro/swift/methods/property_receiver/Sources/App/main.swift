struct Engine {
    func start() {}
}

struct Car {
    let engine: Engine

    func go() {
        engine.start()
        self.engine.start()
    }
}

Car(engine: Engine()).go()
