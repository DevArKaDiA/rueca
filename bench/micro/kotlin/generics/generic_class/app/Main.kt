package app

class Box<T>(val value: T) {
    fun get(): T = value
}

fun unbox(): Int = Box(1).get()
