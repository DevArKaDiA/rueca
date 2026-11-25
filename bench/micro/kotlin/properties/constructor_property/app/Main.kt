package app

class Engine {
    fun start() {}
}

class Car(private val engine: Engine) {
    fun go() = engine.start()
}
