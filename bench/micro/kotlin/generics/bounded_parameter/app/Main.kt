package app

interface Named {
    fun name(): String
}

fun <T : Named> label(t: T): String = t.name()
