package app

class Tag(val name: String, val weight: Int) {
    constructor(name: String) : this(name, 1)

    constructor(weight: Int) : this("w", weight)
}

fun tag() = Tag("a")
