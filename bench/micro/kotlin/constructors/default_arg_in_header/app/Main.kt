package app

fun defaultSize() = 8

class Buffer(val size: Int = defaultSize())

fun buffer() = Buffer()
