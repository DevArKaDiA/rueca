struct Engine {
    func start() {}
}

func drive(_ engine: Engine) {
    engine.start()
}

drive(Engine())
