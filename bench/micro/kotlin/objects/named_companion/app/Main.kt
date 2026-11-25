package app

class Conn {
    companion object Factory {
        fun open(): Conn = Conn()
    }
}

fun a() = Conn.open()

fun b() = Conn.Factory.open()
