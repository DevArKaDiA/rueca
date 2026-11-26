package app

fun <T> identity(x: T): T = x

fun one(): Int = identity(1)
