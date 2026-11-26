package app

class Log {
    fun log(msg: String) {}

    fun log(msg: String, level: Int) {}
}

fun use(l: Log) {
    l.log("a")
    l.log("a", 1)
}
