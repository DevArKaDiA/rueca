package app

class Engine {
    fun start() {}
}

fun ignite(e: Engine?) {
    e?.let { it.start() }
}
