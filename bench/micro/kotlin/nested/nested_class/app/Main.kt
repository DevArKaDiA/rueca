package app

class Outer {
    class Inner {
        fun hi() {}
    }

    fun a() = Inner().hi()
}

fun b() = Outer.Inner().hi()
