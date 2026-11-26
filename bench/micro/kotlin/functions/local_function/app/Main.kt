package app

fun outer(): Int {
    fun inner(x: Int) = x + 1
    return inner(1)
}
