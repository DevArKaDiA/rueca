package app

class Engine {
    fun start() {}
}

fun startAll(engines: List<Engine>) {
    engines.forEach { it.start() }
}
