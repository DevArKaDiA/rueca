package app

class Engine {
    fun start() {}
}

fun ignite(e: Engine?) {
    e?.start()
    e!!.start()
}
