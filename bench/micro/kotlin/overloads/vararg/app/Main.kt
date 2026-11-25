package app

fun total(vararg xs: Int): Int = xs.sum()

fun total(a: Int, b: Int, c: Int, d: Int): Int = a + b + c + d

fun three(): Int = total(1, 2, 3)
