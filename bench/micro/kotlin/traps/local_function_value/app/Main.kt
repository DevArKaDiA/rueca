package app

fun helper(): Int = 2

fun local(): Int {
    val helper = { 1 }
    return helper()
}
