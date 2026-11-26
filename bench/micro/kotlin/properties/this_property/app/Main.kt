package app

class Engine {
    fun start() {}
}

class Car(val engine: Engine) {
    fun go() {
        this.engine.start()
    }
}
