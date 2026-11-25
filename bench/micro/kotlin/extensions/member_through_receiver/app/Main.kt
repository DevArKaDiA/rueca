package app

class Account {
    fun total(): Int = 1
}

fun Account.report(): String = total().toString()
