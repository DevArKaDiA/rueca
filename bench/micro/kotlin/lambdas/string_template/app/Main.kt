package app

class User(val first: String) {
    fun display(): String = first
}

fun card(u: User): String = "name: ${u.display()}"
