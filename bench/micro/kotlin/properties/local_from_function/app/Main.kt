package app

class Engine {
    fun start() {}
}

fun build(): Engine = Engine()

fun drive() {
    val e = build()
    e.start()
}
