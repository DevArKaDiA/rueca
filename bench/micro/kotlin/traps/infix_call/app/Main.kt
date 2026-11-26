package app

infix fun Int.plusOne(o: Int): Int = this + o + 1

fun four(): Int = 1 plusOne 2
