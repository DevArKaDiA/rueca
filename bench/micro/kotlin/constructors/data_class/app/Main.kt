package app

data class User(val name: String)

fun rename(): User = User("a").copy(name = "b")
