package app

open class Base {
    fun helper() {}
}

class Worker : Base() {
    fun run() {
        helper()
    }
}
