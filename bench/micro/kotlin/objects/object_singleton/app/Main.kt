package app

object Registry {
    fun register(name: String) {
        println(name)
    }
}

fun setup() {
    Registry.register("x")
}
