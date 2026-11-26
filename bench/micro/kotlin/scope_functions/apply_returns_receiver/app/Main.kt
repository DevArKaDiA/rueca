package app

class Builder {
    fun size(n: Int): Builder = this

    fun build(): String = "b"
}

fun make(): String = Builder().apply { size(1) }.build()
