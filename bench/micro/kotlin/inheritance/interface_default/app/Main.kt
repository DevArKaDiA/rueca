package app

interface Greeter {
    fun name(): String

    fun greet(): String = "hi " + name()
}

class En : Greeter {
    override fun name() = "en"
}

fun hi(): String = En().greet()
