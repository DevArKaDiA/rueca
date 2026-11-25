package app

fun fact(n: Int): Int = if (n <= 1) 1 else n * fact(n - 1)

fun isEven(n: Int): Boolean = if (n == 0) true else isOdd(n - 1)

fun isOdd(n: Int): Boolean = if (n == 0) false else isEven(n - 1)
