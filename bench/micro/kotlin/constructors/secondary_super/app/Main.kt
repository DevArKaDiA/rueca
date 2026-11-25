package app

open class Base(val n: Int)

class Child : Base {
    constructor(n: Int) : super(n)
}

fun child() = Child(1)
