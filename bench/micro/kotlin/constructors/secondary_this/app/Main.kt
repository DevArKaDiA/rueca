package app

class Pt(val x: Int, val y: Int) {
    constructor(v: Int) : this(v, v)
}

fun diag() = Pt(3)
