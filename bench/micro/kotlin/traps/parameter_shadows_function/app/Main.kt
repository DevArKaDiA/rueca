package app

fun make(): Int = 1

fun produce(make: () -> Int): Int = make()
