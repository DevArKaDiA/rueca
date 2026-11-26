package app

class Printer {
    fun print(s: String) {}

    fun print(i: Int) {}
}

fun number(p: Printer) {
    p.print(42)
}
