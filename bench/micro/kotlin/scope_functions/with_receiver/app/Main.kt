package app

class Engine {
    fun start() {}
}

fun ignite(e: Engine) = with(e) {
    start()
}
