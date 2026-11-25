package app

class Engine {
    fun start() {}
}

class Car {
    val engine: Engine = Engine()

    fun go() {
        engine.start()
    }
}
