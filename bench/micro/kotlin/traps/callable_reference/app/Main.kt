package app

fun inc(x: Int): Int = x + 1

fun incAll(xs: List<Int>): List<Int> = xs.map(::inc)
