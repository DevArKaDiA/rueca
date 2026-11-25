package app

fun boot() {}

class Service {
    init {
        boot()
    }

    fun start() {}
}

fun run() = Service().start()
